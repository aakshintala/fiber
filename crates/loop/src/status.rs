//! The session's own summary of itself (`docs/events.md`, `session_status`).
//!
//! An observer thread folds the session's event stream into a
//! [`SessionStatus`] and emits it, ephemeral, whenever a field changes. It
//! reads the stream through a watcher, so it sees every line the loop writes
//! from every site, once, in order.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak, mpsc};
use std::thread::{Builder, JoinHandle};

use contract::emit::Emit;
use contract::events::{
    CacheLifetime, ContextFill, Event, Git, InputItem, Interaction, PreambleReason, SentTool,
    SessionState, SessionStatus, Waiting, WaitingKind,
};
use contract::shapes::{ContentPart, Origin, Tokens, Usage};
use contract::{ActionId, Envelope, JobId, SessionId};
use log::{Injector, Log, Watcher};
use support::text::one_line;

use crate::Loop;

use crate::usage::Ledger;

/// The control line that ends history: every line before it is folded
/// without emitting.
const LIVE: &str = "status_live";

/// The control line that ends the thread. Both control lines are internal:
/// they go to this watcher alone, never to a client or the log.
const STOP: &str = "status_stop";

/// A classified subject, kept only on this watcher's queue.
const SUBJECT: &str = "status_subject";

/// The control line that marks an accepted `close`: the fold sets
/// `closing` and emits it before the loop answers. A control kind like
/// the others: it goes to the status watcher alone, never to a client or
/// the log.
const CLOSING: &str = "status_closing";

/// The reply half a close request carries: sent once the `session_status`
/// with `closing` is what the log serves, dropped when the observer ends.
type Published = std::sync::mpsc::Sender<()>;

/// What a state is, for deciding when `since` restarts: the state's kind,
/// and the tool name or request id it names.
type Key = (&'static str, Option<String>);

/// Reads the jobs running now.
pub(crate) type Running = Box<dyn Fn() -> Vec<JobId> + Send>;

/// Reads the workspace's branch: `None` outside a git repository.
pub(crate) type Branch = Box<dyn Fn() -> Option<Git> + Send>;

/// Reads the `full` connections attached now.
pub(crate) type Count = Box<dyn Fn() -> u32 + Send>;

/// The fold of the session's lines into one [`SessionStatus`].
pub(crate) struct Fold {
    name: Option<String>,
    /// The first prompt, once the first `turn_started` was seen.
    first_prompt: Option<String>,
    /// Whether a turn has started: only the first turn locks the name.
    turn_seen: bool,
    workspace: String,
    project: String,
    parent: Option<SessionId>,
    supervised: bool,
    model: String,
    thinking: Option<String>,
    window: Option<u64>,
    /// Each requested call's tool name, by action.
    tools: BTreeMap<ActionId, String>,
    /// Started calls not yet completed, in start order.
    running_calls: Vec<ActionId>,
    /// Requests waiting on a person, in the order requested.
    pending: Vec<Waiting>,
    turn_running: bool,
    retrying: bool,
    /// True once the session accepted `close`: nothing clears it.
    closing: bool,
    context: Option<u64>,
    /// The latest cache-pinning request's end, as `ts`: none until the
    /// first qualifying `usage_recorded`.
    cache_ts: Option<u64>,
    /// The preamble's cache lifetime; the countdown counts from the
    /// pinned end through it.
    lifetime: Option<CacheLifetime>,
    /// What the latest preamble built with, for telling an unchanged
    /// resume from one that rebuilt the cache's inputs.
    preamble_tag: Option<PreambleTag>,
    ledger: Ledger,
    spend: Usage,
    /// Delegates started and not yet finished.
    delegates: BTreeSet<JobId>,
    /// The jobs running at the latest read.
    running: Vec<JobId>,
    git: Option<Git>,
    clients: u32,
    /// The status as last computed: `observe` builds it once per folded
    /// line instead of twice.
    last: SessionStatus,
    since: Option<u64>,
    started: u64,
    calls: u64,
    turns: u64,
    step: Option<String>,
    latest: Option<(String, serde_json::Map<String, serde_json::Value>)>,
    pending_subject: Option<(serde_json::Value, Option<String>)>,
    /// Whether history is folded: from then on `running` and `git` are read.
    live: bool,
    read_running: Running,
    read_git: Branch,
    read_clients: Count,
}

impl Fold {
    /// A fold that knows only what the loop was built with.
    pub(crate) fn new(
        project: String,
        workspace: String,
        model: String,
        window: Option<u64>,
        read_running: Running,
        read_git: Branch,
        read_clients: Count,
    ) -> Self {
        let ledger = Ledger::default();
        let spend = ledger.usage();
        let mut fold = Self {
            name: None,
            first_prompt: None,
            turn_seen: false,
            project,
            workspace,
            parent: None,
            supervised: false,
            model,
            thinking: None,
            window,
            tools: BTreeMap::new(),
            running_calls: Vec::new(),
            pending: Vec::new(),
            turn_running: false,
            retrying: false,
            closing: false,
            context: None,
            spend,
            ledger,
            delegates: BTreeSet::new(),
            running: Vec::new(),
            git: None,
            clients: 0,
            last: SessionStatus {
                name: String::new(),
                named: false,
                workspace: String::new(),
                project: String::new(),
                parent: None,
                supervised: false,
                model: String::new(),
                thinking: None,
                state: SessionState::Idle,
                since: 0,
                started: 0,
                calls: 0,
                turns: Some(0),
                step: None,
                git: None,
                context: None,
                cache_expires: None,
                spend: Usage {
                    tokens: Tokens {
                        input: 0,
                        cache_read: 0,
                        cache_write: BTreeMap::new(),
                        output: 0,
                    },
                    cost: Some(0.0),
                    subscription_cost: 0.0,
                },
                closing: false,
                delegates: 0,
                jobs: 0,
                clients: 0,
            },
            since: None,
            started: 0,
            calls: 0,
            cache_ts: None,
            lifetime: None,
            preamble_tag: None,
            turns: 0,
            step: None,
            latest: None,
            pending_subject: None,
            live: false,
            read_running,
            read_git,
            read_clients,
        };
        fold.last = fold.status();
        fold
    }

    /// Accepts `close`: sets `closing` once and recomputes the status.
    /// True when the status changed, as `subject` does.
    pub(crate) fn close(&mut self) -> bool {
        if self.closing {
            return false;
        }
        self.closing = true;
        self.last = self.status();
        true
    }

    /// History is folded: reads the jobs and the branch now, and from here
    /// at the lines that change them.
    pub(crate) fn go_live(&mut self) {
        self.live = true;
        self.running = (self.read_running)();
        self.git = (self.read_git)();
        self.clients = (self.read_clients)();
        self.last = self.status();
    }

    /// Folds `line`. True when a field of the status changed.
    pub(crate) fn observe(&mut self, line: &Envelope) -> bool {
        // An ephemeral line is a delta, apart from the retry notice and the
        // connection count.
        if !line.is_durable() && line.kind != "retry_scheduled" && line.kind != "clients" {
            return false;
        }
        // A line that does not read is skipped: the fold never panics.
        let Ok(Some(event)) = Event::from_envelope(line) else {
            return false;
        };
        let key = self.key();
        self.apply(&event, line);
        if self.live {
            self.clients = (self.read_clients)();
        }
        if self.since.is_none() || self.key() != key {
            self.since = Some(line.ts);
        }
        let status = self.status();
        let changed = status != self.last;
        self.last = status;
        changed
    }

    /// Holds the latest classification even when its request is still in
    /// the lagged watcher's history. Equal call payloads have equal subjects.
    pub(crate) fn subject(&mut self, line: &Envelope) -> bool {
        let Some(call) = line.payload.get("call") else {
            return false;
        };
        let subject = line
            .payload
            .get("subject")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        self.pending_subject = Some((call.clone(), subject.clone()));
        if let Some((name, payload)) = &self.latest
            && call.as_object() == Some(payload)
        {
            self.step = Some(call_step(name, subject.as_deref()));
        }
        let status = self.status();
        let changed = status != self.last;
        self.last = status;
        changed
    }

    fn apply(&mut self, event: &Event, line: &Envelope) {
        match event {
            Event::SessionStarted(started) => {
                self.started = line.ts;
                self.workspace.clone_from(&started.workspace);
                self.parent = started.parent.as_ref().map(|p| p.session_id.clone());
                self.supervised = started.supervised;
            }
            Event::SessionNamed(named) => self.name.clone_from(&named.name),
            Event::PreambleBuilt(built) => {
                self.model.clone_from(&built.model);
                self.thinking.clone_from(&built.thinking);
                self.window = Some(built.context_window);
                // A process that built its preamble no longer waits on an offer.
                self.pending.retain(|p| p.kind != WaitingKind::Offer);
                // The countdown counts through the latest preamble's
                // lifetime. A resume that rebuilt the cache's inputs
                // starts a cold cache; an unchanged one keeps the old
                // chain alive, so the pin stays.
                self.lifetime = Some(built.cache_lifetime);
                let tag = PreambleTag::of(built);
                if built.reason == PreambleReason::Resume
                    && self.preamble_tag.as_ref().is_some_and(|prev| *prev != tag)
                {
                    self.cache_ts = None;
                }
                self.preamble_tag = Some(tag);
            }
            Event::ModelChanged(changed) => {
                self.model.clone_from(&changed.after.model);
                self.thinking.clone_from(&changed.after.thinking);
                // A vendor holds its cache per account or workspace, so
                // the new label starts with a cold cache
                // (`docs/prompt-cache.md`, "Switching model").
                self.cache_ts = None;
            }
            Event::TurnStarted(started) => {
                if line.turn_id.is_some() {
                    self.turns += 1;
                }
                if self.first_prompt.is_none() {
                    if let Some(prompt) = prompt_of(&started.input) {
                        self.first_prompt = Some(prompt);
                    } else if !self.turn_seen
                        && !started
                            .input
                            .iter()
                            .any(|item| matches!(item, InputItem::Message { .. }))
                    {
                        // A first turn with no message at all leaves no
                        // name to fall back on; a turn of others'
                        // messages waits for a driver prompt.
                        self.first_prompt = Some(String::new());
                    }
                }
                self.turn_seen = true;
                self.turn_running = true;
                self.retrying = false;
                self.refresh_running();
                if self.live {
                    self.git = (self.read_git)();
                }
            }
            Event::TurnCompleted(_) => {
                self.turn_running = false;
                self.retrying = false;
                // A completed turn's calls are named nowhere further:
                // pending summaries were stored at request time, and no
                // running call or turn is left to name.
                self.tools.clear();
                self.running_calls.clear();
                self.pending.clear();
                self.refresh_running();
            }
            Event::RetryScheduled(_) => self.retrying = true,
            Event::AssistantMessageStarted(_) | Event::StepStarted(_) => self.retrying = false,
            Event::ToolCallRequested(call) => {
                self.calls += 1;
                self.step = Some(one_line(&call.name));
                self.latest = Some((call.name.clone(), line.payload.clone()));
                if let Some((payload, subject)) = &self.pending_subject
                    && payload.as_object() == Some(&line.payload)
                {
                    self.step = Some(call_step(&call.name, subject.as_deref()));
                }
                if let Some(id) = &line.action_id {
                    self.tools.insert(id.clone(), call.name.clone());
                }
            }
            Event::ToolCallStarted(_) => {
                if let Some(id) = &line.action_id {
                    self.running_calls.push(id.clone());
                }
            }
            Event::ToolCallCompleted(_) => {
                if let Some(id) = &line.action_id {
                    self.running_calls.retain(|call| call != id);
                    // A completed call is named nowhere further: it is
                    // no longer running, and its approval was requested
                    // before it started.
                    self.tools.remove(id);
                }
            }
            Event::PermissionRequested(asked) => {
                let summary = self.tool_of(line.action_id.as_ref());
                self.pending.push(Waiting {
                    request_id: asked.request_id.clone(),
                    kind: WaitingKind::Approval,
                    summary,
                });
            }
            Event::PermissionResolved(resolved) => {
                if let Some(id) = &resolved.request_id {
                    self.pending.retain(|p| p.request_id != *id);
                }
            }
            Event::InteractionRequested(asked) => {
                let text = match &asked.interaction {
                    Interaction::Form { fields } => fields.first().map(|f| f.question.clone()),
                    Interaction::Confirm { prompt }
                    | Interaction::Select { prompt, .. }
                    | Interaction::MultiSelect { prompt, .. }
                    | Interaction::TextInput { prompt } => Some(prompt.clone()),
                }
                .unwrap_or_default();
                let summary = if text.is_empty() {
                    self.tool_of(asked.action_ids.as_ref().and_then(|ids| ids.first()))
                } else {
                    text
                };
                self.pending.push(Waiting {
                    request_id: asked.request_id.clone(),
                    kind: WaitingKind::Question,
                    summary: one_line(&summary),
                });
            }
            Event::InteractionResolved(resolved) => {
                self.pending.retain(|p| p.request_id != resolved.request_id);
            }
            // The offer comes before any approval its process raises, so it
            // is shown first. A re-raise adds a second entry with the same
            // id; the resolution removes both.
            Event::RepositoryCodeOffered(offered) => {
                self.pending.insert(
                    0,
                    Waiting {
                        request_id: offered.request_id.clone(),
                        kind: WaitingKind::Offer,
                        summary: offer_summary(offered.items.len()),
                    },
                );
            }
            Event::RepositoryCodeResolved(resolved) => {
                self.pending.retain(|p| p.request_id != resolved.request_id);
            }
            Event::UsageRecorded(recorded) => {
                let corrected = self.ledger.record(recorded);
                self.spend = self.ledger.usage();
                // A session-model reply: not the reviewer's (no action), a
                // delegate's copy or an extension's call. A late correction
                // of an earlier call is not the context's latest size, and a
                // record that reported no input tokens measured nothing
                // (`docs/events.md`, `session_status`).
                if !corrected
                    && line.action_id.is_some()
                    && recorded.extension.is_none()
                    && recorded.origin_session_id.is_none()
                    && crate::handoff::prompt_tokens(&recorded.tokens) > 0
                {
                    self.context = Some(crate::handoff::context_tokens(&recorded.tokens));
                }
                // The cache pin moves on a conversation request (an
                // envelope action of the session's own, not an
                // extension's) or a warming refresh, never on a
                // correction, a copy, a reviewer's call or a notes
                // proposal, which carries no action and no marker.
                let conversation = line.action_id.is_some()
                    && recorded.extension.is_none()
                    && recorded.reviewer.is_none();
                let refresh = recorded.warming == Some(true) && recorded.reviewer.is_none();
                if !corrected && recorded.origin_session_id.is_none() && (conversation || refresh) {
                    self.cache_ts = Some(line.ts);
                }
            }
            Event::HandoffCompleted(_) => {
                self.context = None;
                // The reviewer misses the cache once, as the model does
                // (`docs/prompt-cache.md`, "Rules for other areas").
                self.cache_ts = None;
            }
            // A reload rebuilds the preamble from the start, so the old
            // cache entry is dead (`docs/prompt-cache.md`, "Reload").
            Event::Reloaded(_) => self.cache_ts = None,
            Event::JobStarted(_) => self.refresh_running(),
            // A delegate's end removes it from the status fold, whether
            // its `delegate_finished` or only its `job_completed` was
            // seen: an orphaned delegate resumed without its finish would
            // otherwise stay listed forever.
            Event::JobCompleted(completed) => {
                self.delegates.remove(&completed.job_id);
                self.refresh_running();
            }
            Event::DelegateStarted(started) => {
                self.delegates.insert(started.job_id.clone());
            }
            Event::DelegateFinished(finished) => {
                self.delegates.remove(&finished.job_id);
            }
            // A process boundary: a call the last process left running is
            // not running now. A pending approval stays: the resumed turn
            // finishes it.
            Event::FiberStarted(_) => {
                self.running_calls.clear();
                self.retrying = false;
            }
            Event::FiberExited(_)
            | Event::Rewound(_)
            | Event::Moved(_)
            | Event::SteeringApplied(_)
            | Event::SteeringQueue(_)
            | Event::ShellCommand(_)
            | Event::Clients(_)
            | Event::SessionStatus(_)
            | Event::ContextAdded(_)
            | Event::AssistantMessageDelta(_)
            | Event::AssistantMessageCompleted(_)
            | Event::TextCompleted(_)
            | Event::ToolCallArgumentsDelta(_)
            | Event::ReasoningStarted(_)
            | Event::ReasoningDelta(_)
            | Event::ReasoningCompleted(_)
            | Event::ToolCallDelta(_)
            | Event::QuotaNoticed(_)
            | Event::QuotaRearmed(_)
            | Event::Notice(_)
            | Event::OpeningMessage(_)
            | Event::InstructionFile(_)
            | Event::DateChanged(_)
            | Event::SkillsChanged(_)
            | Event::HandoffStarted(_)
            | Event::SkillsResent(_)
            | Event::ContextNudged(_)
            | Event::ReviewerKept(_)
            | Event::McpServerFailed(_)
            | Event::McpServerReady(_)
            | Event::ExtensionsLoaded(_)
            | Event::ExtensionStateSet(_)
            | Event::ExtensionStateUnset(_)
            | Event::ExtensionUi(_)
            | Event::ExtensionMessage(_)
            | Event::ExtensionLog(_)
            | Event::ExtensionHello(_)
            | Event::ExtensionRequest(_)
            | Event::ExtensionExec(_)
            | Event::JobDelta(_)
            | Event::JobLine(_)
            | Event::JobsPendingNotified(_)
            | Event::CommandAccepted(_)
            | Event::DirectoryAdded(_)
            | Event::CommandRejected(_) => {}
        }
    }

    fn refresh_running(&mut self) {
        if self.live {
            self.running = (self.read_running)();
        }
    }

    /// The tool a call named; empty when the call is unknown.
    fn tool_of(&self, action: Option<&ActionId>) -> String {
        action
            .and_then(|id| self.tools.get(id))
            .cloned()
            .unwrap_or_default()
    }

    /// The precedence order: a pending request, a running call, a turn, then
    /// the jobs.
    fn state(&self) -> SessionState {
        if let Some(first) = self.pending.first() {
            return SessionState::Waiting {
                waiting: first.clone(),
            };
        }
        if let Some(call) = self.running_calls.last() {
            return SessionState::Tool {
                tool: self.tool_of(Some(call)),
            };
        }
        if self.turn_running {
            return if self.retrying {
                SessionState::Retrying
            } else {
                SessionState::Streaming
            };
        }
        if self.running.is_empty() {
            SessionState::Idle
        } else {
            SessionState::Jobs
        }
    }

    fn key(&self) -> Key {
        match self.state() {
            SessionState::Streaming => ("streaming", None),
            SessionState::Tool { tool } => ("tool", Some(tool)),
            SessionState::Retrying => ("retrying", None),
            SessionState::Waiting { waiting } => ("waiting", Some(waiting.request_id.0)),
            SessionState::Jobs => ("jobs", None),
            SessionState::Idle => ("idle", None),
        }
    }

    /// The status line as the fold stands.
    pub(crate) fn status(&self) -> SessionStatus {
        let jobs = self
            .running
            .iter()
            .filter(|id| !self.delegates.contains(id))
            .count();
        SessionStatus {
            named: self.name.is_some(),
            name: self
                .name
                .clone()
                .or_else(|| self.first_prompt.clone())
                .unwrap_or_default(),
            workspace: self.workspace.clone(),
            project: self.project.clone(),
            parent: self.parent.clone(),
            supervised: self.supervised,
            model: self.model.clone(),
            thinking: self.thinking.clone(),
            state: self.state(),
            closing: self.closing,
            since: self.since.unwrap_or_default(),
            started: self.started,
            calls: self.calls,
            turns: Some(self.turns),
            step: self.step.clone(),
            git: self.git.clone(),
            // A window of 0 is no window: a fill against it means nothing.
            context: self
                .context
                .zip(self.window.filter(|window| *window > 0))
                .map(|(tokens, window)| ContextFill { tokens, window }),
            // The pinned request's end plus the folded lifetime,
            // saturating: absent until the first qualifying request.
            cache_expires: self
                .cache_ts
                .zip(self.lifetime)
                .map(|(ts, lifetime)| ts.saturating_add(lifetime_ms(lifetime))),
            spend: self.spend.clone(),
            delegates: u32::try_from(self.delegates.len()).unwrap_or(u32::MAX),
            jobs: u32::try_from(jobs).unwrap_or(u32::MAX),
            clients: self.clients,
        }
    }
}

/// The text of a turn's first driver message, its text parts in order;
/// `None` when the turn started with no driver message.
fn prompt_of(input: &[InputItem]) -> Option<String> {
    let content = input.iter().find_map(|item| match item {
        InputItem::Message {
            content, sender, ..
        } if sender.origin == Origin::Driver => Some(content),
        InputItem::Message { .. }
        | InputItem::ShellCommand { .. }
        | InputItem::Jobs { .. }
        | InputItem::Handoff { .. }
        | InputItem::Rewind { .. }
        | InputItem::Unknown => None,
    });
    content.map(|content| {
        content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
            })
            .collect()
    })
}

/// What a preamble built with that decides whether a resume kept the
/// cache's chain alive: the settings, the system prompt and the tool
/// definitions. Anything else (a handoff point, a budget, a window)
/// leaves the cached prefix alone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreambleTag {
    model: String,
    thinking: Option<String>,
    cache_lifetime: CacheLifetime,
    credential: Option<String>,
    tool_choice: String,
    system_prompt: String,
    tools: Vec<SentTool>,
}

impl PreambleTag {
    fn of(built: &contract::events::PreambleBuilt) -> Self {
        Self {
            model: built.model.clone(),
            thinking: built.thinking.clone(),
            cache_lifetime: built.cache_lifetime,
            credential: built.credential.clone(),
            tool_choice: built.tool_choice.clone(),
            system_prompt: built.system_prompt.clone(),
            tools: built.tools.clone(),
        }
    }
}

/// A cache lifetime in milliseconds, as `Loop::lifetime` maps it
/// (`crates/loop/src/warm.rs`).
fn lifetime_ms(lifetime: CacheLifetime) -> u64 {
    match lifetime {
        CacheLifetime::FiveMinutes => 300_000,
        CacheLifetime::OneHour => 3_600_000,
    }
}

/// A tool with no usable subject shows its name alone.
fn call_step(name: &str, subject: Option<&str>) -> String {
    match subject.filter(|subject| !subject.is_empty()) {
        Some(subject) => one_line(&format!("{name} {subject}")),
        None => one_line(name),
    }
}

/// Classification's private path to the status watcher, and the request
/// half of the close handshake; neither is ever logged.
#[derive(Clone)]
pub(crate) struct Tap {
    injector: Injector,
    closing: mpsc::Sender<Published>,
    /// Test-only: fired once `closing` has queued its request, so a test
    /// can order the observer's end after the queue. Never read in
    /// production builds.
    #[cfg(test)]
    queued: Option<mpsc::Sender<()>>,
}

impl Tap {
    /// Queues the subject with exactly the call payload the log wrote.
    pub(crate) fn subject(
        &self,
        call: &contract::events::ToolCallRequested,
        subject: Option<&str>,
    ) {
        let Ok(call) = serde_json::to_value(call) else {
            return;
        };
        let mut line = control(SUBJECT);
        line.payload.insert("call".to_owned(), call);
        line.payload.insert("subject".to_owned(), subject.into());
        self.injector.push_kept(line);
    }

    /// Test-only: `tx` fires once `closing` has queued its request.
    #[cfg(test)]
    pub(crate) fn announce_queued(&mut self, tx: mpsc::Sender<()>) {
        self.queued = Some(tx);
    }

    /// Marks the accepted `close`: queues the control line, then waits
    /// until the `session_status` carrying `closing` is written, or the
    /// observer has ended. A dropped request sender means the observer is
    /// gone, so the loop answers at once; a dropped reply means the
    /// observer ended before it answered, so the loop answers too.
    pub(crate) fn closing(&self) {
        let (reply, published) = mpsc::channel();
        if self.closing.send(reply).is_err() {
            return;
        }
        #[cfg(test)]
        if let Some(queued) = &self.queued {
            // The test's own wait fails first and drops the receiver,
            // which releases this send.
            match queued.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
        self.injector.push_kept(control(CLOSING));
        match published.recv() {
            Ok(()) | Err(_) => {}
        }
    }
}

/// The running observer thread.
pub(crate) struct Status {
    injector: Injector,
    closing: mpsc::Sender<Published>,
    thread: JoinHandle<()>,
}

impl Status {
    /// A sender for this observer's private control queue.
    pub(crate) fn tap(&self) -> Tap {
        Tap {
            injector: self.injector.clone(),
            closing: self.closing.clone(),
            #[cfg(test)]
            queued: None,
        }
    }

    /// Ends the thread after every line already written, and waits for it:
    /// no `session_status` follows this call.
    pub(crate) fn stop(self) {
        self.signal();
        self.join();
    }

    /// Queues the stop line, after every line already written.
    fn signal(&self) {
        self.injector.push_kept(control(STOP));
    }

    /// Waits for the thread to end.
    fn join(self) {
        // A thread that panicked has nothing left to write.
        match self.thread.join() {
            Ok(()) | Err(_) => {}
        }
    }
}

fn control(kind: &str) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(String::new()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::Map::new(),
    }
}

/// Starts the observer for `looped`'s session: it folds the log from the
/// start, so a resumed session's history is one status, then follows it. A
/// thread that cannot start leaves the session with no status. A first page
/// that does not parse folds the lines before the failure, then the follow
/// loop ends at the failure as it does for a later page; only a line
/// damaged after [`Log::open`] read it, or a line of a kind open does not
/// fold that is JSON but not an event line, reaches this.
pub(crate) fn spawn(looped: &Loop) -> Option<Status> {
    let jobs = looped.ending.jobs.clone();
    let workspace = looped.workspace.clone();
    let project = project_of(looped.log.dir());
    let weak = Arc::downgrade(&looped.log);
    let fold = Fold::new(
        project,
        looped.workspace_label.clone(),
        looped.model.reference.clone(),
        Some(looped.prompt.context_window),
        Box::new(move || jobs.as_ref().map(|jobs| jobs.running()).unwrap_or_default()),
        Box::new(move || branch(&workspace)),
        Box::new(move || clients_of(&weak)),
    );
    start(&looped.log, fold)
}

/// Registers the watcher and starts the thread that folds it with `fold`.
fn start(log: &Arc<Log>, fold: Fold) -> Option<Status> {
    let weak = Arc::downgrade(log);
    let mut watcher = log.watch_all();
    let injector = watcher.injector();
    injector.push_kept(control(LIVE));
    let (closing, requests) = mpsc::channel::<Published>();
    let thread = Builder::new()
        .name("status".to_owned())
        .spawn(move || {
            let mut observer = Observer {
                fold,
                log: weak,
                live: false,
                last: None,
                requests,
            };
            follow(&mut watcher, &mut observer);
        })
        .ok()?;
    Some(Status {
        injector,
        closing,
        thread,
    })
}

/// Folds the watcher's lines until the stop line, the end of the log, or a
/// log that is gone.
fn follow(watcher: &mut Watcher, observer: &mut Observer) {
    while let Ok(Some(line)) = watcher.recv() {
        match observer.line(&line) {
            Flow::Go => {}
            Flow::Stop => {
                // The stop line is kept, so it can come before durable
                // lines a lagging watcher has not read yet: fold every one
                // already written before the thread ends.
                while let Ok(Some(line)) = watcher.try_recv() {
                    if matches!(observer.line(&line), Flow::Gone) {
                        return;
                    }
                }
                return;
            }
            Flow::Gone => return,
        }
    }
}

/// What the thread does after a line.
enum Flow {
    Go,
    /// The stop line.
    Stop,
    /// The log is gone.
    Gone,
}

/// The thread's state: the fold and what was last emitted.
struct Observer {
    fold: Fold,
    log: Weak<Log>,
    live: bool,
    last: Option<SessionStatus>,
    requests: mpsc::Receiver<Published>,
}

impl Observer {
    fn line(&mut self, line: &Envelope) -> Flow {
        let changed = match line.kind.as_str() {
            STOP => return Flow::Stop,
            SUBJECT => self.fold.subject(line),
            CLOSING => {
                let changed = self.fold.close();
                if self.live && changed && !self.emit() {
                    return Flow::Gone;
                }
                if let Ok(reply) = self.requests.try_recv() {
                    match reply.send(()) {
                        Ok(()) | Err(_) => {}
                    }
                }
                return Flow::Go;
            }
            LIVE => {
                self.live = true;
                self.fold.go_live();
                true
            }
            _ => self.fold.observe(line),
        };
        if self.live && changed && !self.emit() {
            return Flow::Gone;
        }
        Flow::Go
    }

    /// Emits the fold's status when it differs from the last one emitted.
    /// False once the log is gone.
    fn emit(&mut self) -> bool {
        let status = self.fold.status();
        if self.last.as_ref() == Some(&status) {
            return true;
        }
        // The log is held only while the line is written.
        let Some(log) = self.log.upgrade() else {
            return false;
        };
        log.emit(&Event::SessionStatus(status.clone()));
        self.last = Some(status);
        true
    }
}

/// The project key a session directory sits under: the name of its
/// grandparent (`projects/<key>/sessions/<id>`); `""` when it has none.
fn project_of(dir: &Path) -> String {
    dir.parent()
        .and_then(|sessions| sessions.parent())
        .and_then(|project| project.file_name())
        .map(|key| key.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What an offer of `items` items says while it waits.
fn offer_summary(items: usize) -> String {
    if items == 1 {
        "1 item from the repository".to_owned()
    } else {
        format!("{items} items from the repository")
    }
}

/// The `count` of the log's latest `clients` line; 0 when there is none,
/// it does not read as `clients`, or the log is gone.
pub(crate) fn clients_of(log: &Weak<Log>) -> u32 {
    let Some(log) = log.upgrade() else {
        return 0;
    };
    let Some(line) = log.latest("clients") else {
        return 0;
    };
    match Event::from_envelope(&line) {
        Ok(Some(Event::Clients(clients))) => clients.count,
        Ok(_) | Err(_) => 0,
    }
}

/// The workspace's branch: `HEAD` is a detached head, and a failed or
/// absent `git`, or a directory outside a repository, is no repository.
fn branch(workspace: &PathBuf) -> Option<Git> {
    let out = support::git::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Some(Git {
        branch: (name != "HEAD").then_some(name),
    })
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
