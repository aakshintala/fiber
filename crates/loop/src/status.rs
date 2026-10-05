//! The session's own summary of itself (`docs/events.md`, `session_status`).
//!
//! An observer thread folds the session's event stream into a
//! [`SessionStatus`] and emits it, ephemeral, whenever a field changes. It
//! reads the stream through a watcher, so it sees every line the loop writes
//! from every site, once, in order.

use std::collections::{BTreeMap, BTreeSet};

use contract::events::{
    ContextFill, Event, Git, InputItem, Interaction, SessionState, SessionStatus, Waiting,
    WaitingKind,
};
use contract::shapes::{ContentPart, Usage};
use contract::{ActionId, Envelope, JobId, RequestId, SessionId};

use crate::usage::Ledger;

/// What a pending request is, in the order requested.
struct Pending {
    request_id: RequestId,
    kind: WaitingKind,
    summary: String,
}

/// What a state is, for deciding when `since` restarts: the state's kind,
/// and the tool name or request id it names.
type Key = (&'static str, Option<String>);

/// Reads the jobs running now.
pub(crate) type Running = Box<dyn Fn() -> Vec<JobId> + Send>;

/// Reads the workspace's branch: `None` outside a git repository.
pub(crate) type Branch = Box<dyn Fn() -> Option<Git> + Send>;

/// The fold of the session's lines into one [`SessionStatus`].
pub(crate) struct Fold {
    name: Option<String>,
    /// The first prompt, once the first `turn_started` was seen.
    first_prompt: Option<String>,
    workspace: String,
    parent: Option<SessionId>,
    model: String,
    window: Option<u64>,
    /// Each requested call's tool name, by action.
    tools: BTreeMap<ActionId, String>,
    /// Started calls not yet completed, in start order.
    running_calls: Vec<ActionId>,
    pending: Vec<Pending>,
    turn_running: bool,
    retrying: bool,
    context: Option<u64>,
    ledger: Ledger,
    spend: Usage,
    /// Delegates started and not yet finished.
    delegates: BTreeSet<JobId>,
    /// The jobs running at the latest read.
    running: Vec<JobId>,
    git: Option<Git>,
    since: Option<u64>,
    /// Whether history is folded: from then on `running` and `git` are read.
    live: bool,
    read_running: Running,
    read_git: Branch,
}

impl Fold {
    /// A fold that knows only what the loop was built with.
    pub(crate) fn new(
        workspace: String,
        model: String,
        window: Option<u64>,
        read_running: Running,
        read_git: Branch,
    ) -> Self {
        let ledger = Ledger::default();
        Self {
            name: None,
            first_prompt: None,
            workspace,
            parent: None,
            model,
            window,
            tools: BTreeMap::new(),
            running_calls: Vec::new(),
            pending: Vec::new(),
            turn_running: false,
            retrying: false,
            context: None,
            spend: ledger.usage(),
            ledger,
            delegates: BTreeSet::new(),
            running: Vec::new(),
            git: None,
            since: None,
            live: false,
            read_running,
            read_git,
        }
    }

    /// History is folded: reads the jobs and the branch now, and from here
    /// at the lines that change them.
    pub(crate) fn go_live(&mut self) {
        self.live = true;
        self.running = (self.read_running)();
        self.git = (self.read_git)();
    }

    /// Folds `line`. True when a field of the status changed.
    pub(crate) fn observe(&mut self, line: &Envelope) -> bool {
        // An ephemeral line is a delta, apart from the retry notice.
        if !line.is_durable() && line.kind != "retry_scheduled" {
            return false;
        }
        // A line that does not read is skipped: the fold never panics.
        let Ok(Some(event)) = Event::from_envelope(line) else {
            return false;
        };
        let before = self.status();
        let key = self.key();
        self.apply(&event, line);
        if self.since.is_none() || self.key() != key {
            self.since = Some(line.ts);
        }
        self.status() != before
    }

    fn apply(&mut self, event: &Event, line: &Envelope) {
        match event {
            Event::SessionStarted(started) => {
                self.workspace.clone_from(&started.workspace);
                self.parent = started.parent.as_ref().map(|p| p.session_id.clone());
            }
            Event::SessionNamed(named) => self.name.clone_from(&named.name),
            Event::PreambleBuilt(built) => {
                self.model.clone_from(&built.model);
                self.window = Some(built.context_window);
            }
            Event::ModelChanged(changed) => self.model.clone_from(&changed.after.model),
            Event::TurnStarted(started) => {
                if self.first_prompt.is_none() {
                    self.first_prompt = Some(prompt_of(&started.input));
                }
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
                self.running_calls.clear();
                self.pending.clear();
                self.refresh_running();
            }
            Event::RetryScheduled(_) => self.retrying = true,
            Event::AssistantMessageStarted(_) | Event::StepStarted(_) => self.retrying = false,
            Event::ToolCallRequested(call) => {
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
                }
            }
            Event::PermissionRequested(asked) => {
                let summary = self.tool_of(line.action_id.as_ref());
                self.pending.push(Pending {
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
                self.pending.push(Pending {
                    request_id: asked.request_id.clone(),
                    kind: WaitingKind::Question,
                    summary: one_line(&summary),
                });
            }
            Event::InteractionResolved(resolved) => {
                self.pending.retain(|p| p.request_id != resolved.request_id);
            }
            Event::UsageRecorded(recorded) => {
                self.ledger.record(recorded);
                self.spend = self.ledger.usage();
                // A session-model reply: not the reviewer's (no action), a
                // delegate's copy or an extension's call.
                if line.action_id.is_some()
                    && recorded.extension.is_none()
                    && recorded.origin_session_id.is_none()
                {
                    self.context = Some(crate::handoff::context_tokens(&recorded.tokens));
                }
            }
            Event::HandoffCompleted(_) => self.context = None,
            Event::JobStarted(_) | Event::JobCompleted(_) => self.refresh_running(),
            Event::DelegateStarted(started) => {
                self.delegates.insert(started.job_id.clone());
            }
            Event::DelegateFinished(finished) => {
                self.delegates.remove(&finished.job_id);
            }
            Event::FiberStarted(_)
            | Event::FiberExited(_)
            | Event::Rewound(_)
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
            | Event::RepositoryCodeOffered(_)
            | Event::RepositoryCodeResolved(_)
            | Event::QuotaNoticed(_)
            | Event::Notice(_)
            | Event::OpeningMessage(_)
            | Event::InstructionFile(_)
            | Event::DateChanged(_)
            | Event::SkillsChanged(_)
            | Event::HandoffStarted(_)
            | Event::SkillsResent(_)
            | Event::ContextNudged(_)
            | Event::McpServerFailed(_)
            | Event::McpServerReady(_)
            | Event::Reloaded(_)
            | Event::ExtensionsLoaded(_)
            | Event::ExtensionStateSet(_)
            | Event::ExtensionStateUnset(_)
            | Event::ExtensionUi(_)
            | Event::ExtensionMessage(_)
            | Event::ExtensionExec(_)
            | Event::JobDelta(_)
            | Event::JobLine(_)
            | Event::JobsPendingNotified(_)
            | Event::CommandAccepted(_)
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
                waiting: Waiting {
                    request_id: first.request_id.clone(),
                    kind: first.kind,
                    summary: first.summary.clone(),
                },
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
            name: self
                .name
                .clone()
                .or_else(|| self.first_prompt.clone())
                .unwrap_or_default(),
            workspace: self.workspace.clone(),
            parent: self.parent.clone(),
            model: self.model.clone(),
            state: self.state(),
            since: self.since.unwrap_or_default(),
            git: self.git.clone(),
            // A window of 0 is no window: a fill against it means nothing.
            context: self
                .context
                .zip(self.window.filter(|window| *window > 0))
                .map(|(tokens, window)| ContextFill { tokens, window }),
            spend: self.spend.clone(),
            delegates: u32::try_from(self.delegates.len()).unwrap_or(u32::MAX),
            jobs: u32::try_from(jobs).unwrap_or(u32::MAX),
        }
    }
}

/// The text of a turn's first message item, its text parts in order; empty
/// when the turn started with no message.
fn prompt_of(input: &[InputItem]) -> String {
    let content = input.iter().find_map(|item| match item {
        InputItem::Message { content, .. } => Some(content),
        InputItem::ShellCommand { .. }
        | InputItem::Jobs { .. }
        | InputItem::Handoff { .. }
        | InputItem::Unknown => None,
    });
    content
        .into_iter()
        .flatten()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect()
}

/// `text` on one line.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
