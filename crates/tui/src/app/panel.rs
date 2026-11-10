//! The session screen's side panel state: the attached session's folded
//! data behind a small interface (`docs/tui.md`, "The panel"). The app
//! calls [`App::panel_line`] for every attached-session line and reads the
//! fold through [`PanelState`]'s accessors; drawing lives in
//! `crate::view::panel`.

mod quota;

use contract::events::EventKind;
use std::collections::{BTreeMap, BTreeSet};

use contract::Envelope;
use contract::JobId;
use contract::SessionId;
use contract::events::Outcome;
use contract::events::{
    CommandAccepted, CommandRejected, CommandResult, DelegateStarted, ExtensionUi, FileChange,
    JobCompleted, JobStarted, McpServerFailed, McpServerReady, ModelChanged, PreambleBuilt,
    SessionStarted, SessionState, SessionStatus, ToolCallCompleted, Ui, UsageRecorded,
};

use super::{App, Effect, Kind, mint};
use crate::context_view::Compaction;
use crate::input::Draft;
use crate::shell;
use serde_json::Value;

/// A panel item that does something when clicked (`docs/tui.md`, "The panel", "Git").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// The Jobs line: lists the running jobs.
    Jobs,
    /// The Session card's branch row: runs `git status`.
    Branch,
    /// The Session card's "N waiting" while the rail is not drawn: shows
    /// the rail (`docs/tui.md`, "Shedding").
    Waiting,
    /// The Session card's model line: opens the model picker.
    Model,
    /// The Session card's context rows: opens the context breakdown.
    Context,
    /// The status row's working directory and permission mode: opens the Session status card's view (`docs/tui.md`, "The input box").
    Status,
    /// The Session card's spend rows: opens the usage view (`docs/tui.md`,
    /// "The panel").
    Usage,
    /// The Quota card's rows and the narrow layout's Q segment: opens the
    /// quota view and fetches every label's quota (`docs/tui.md`,
    /// "The panel").
    Quota,
    /// A Changed files card row, by its rank in `changed_files_view::ranked`.
    File(usize),
    /// The Delegates card's finished fold row.
    DelegateFold,
    /// A Delegates card row, by its job's serial: a press and a release
    /// on the same serial opens that delegate, never the row's position.
    Delegate(u64),
    /// A Jobs card row, by its job's serial: a press and a release on the
    /// same serial opens that job, never the row's position.
    Job(u64),
    /// The narrow layout's "N delegates running": lists the running
    /// delegates (`docs/tui.md`, "The narrow layout").
    DelegateList,
    /// The narrow layout's "N jobs running": lists the running jobs.
    JobList,
    /// The Changed files card's totals row: opens the file list.
    ChangedFiles,
    /// The Session card's tools line: opens the tools view (`docs/tui.md`,
    /// "The panel").
    Tools,
    /// The narrow layout's widget row: expands or collapses it.
    Widget,
}

/// What the branch query last answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Branch {
    /// On a branch.
    Named(String),
    /// `HEAD`: detached.
    Detached,
    /// No branch to show.
    Absent,
}

/// An extension's widget: its latest lines, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Widget {
    /// The extension's name.
    pub(crate) extension: String,
    /// The widget's id.
    pub(crate) widget: String,
    /// Its latest lines.
    pub(crate) lines: Vec<String>,
}

/// Whether the running model call is streaming thinking. Only a
/// streamed `reasoning_delta` sets `Thinking`; the first text delta,
/// the first `tool_call_arguments_delta`, or the call's end latches
/// `Closed` for the rest of that call, and the next call starts back at
/// `Idle` (`docs/tui.md`, "The panel").
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ThinkingState {
    /// No call is streaming thinking.
    #[default]
    Idle,
    /// The running call is streaming thinking: the Session card draws
    /// its dim `∴` after the thinking level.
    Thinking,
    /// This call stopped thinking: later reasoning deltas stay ignored
    /// until the next call starts.
    Closed,
}

/// The session figures one fold keeps: the attached session's or one
/// open delegate's. The attached fold and the delegate fold share one
/// fold over these, fed the selected session, so the two never drift
/// (`docs/tui.md`, "Swapped views").
#[derive(Debug, Default)]
struct SessionFigures {
    status: Option<SessionStatus>,
    model: Option<String>,
    thinking: Option<String>,
    thinking_state: ThinkingState,
    /// The credential label the latest `preamble_built` or
    /// `model_changed` names: `None` before either, `Some(None)` when
    /// the provider takes no credential.
    credential: Option<Option<String>>,
    window: Option<u64>,
    trigger_at: Option<u64>,
    budget: Option<f64>,
    turns: u64,
    started_at: Option<u64>,
    speed: Option<u64>,
    down: BTreeSet<String>,
}

/// Folds one session-figure envelope into `figures`: `session_status`,
/// `preamble_built`, `model_changed`, `turn_completed`,
/// `assistant_message_started`, the thinking latches,
/// `usage_recorded`, and the MCP down/ready lines. Any other envelope
/// leaves `figures` alone, so each caller routes only its own
/// session's lines here (`docs/tui.md`, "Swapped views").
fn fold_session_figures(figures: &mut SessionFigures, envelope: &Envelope) {
    match envelope.kind() {
        EventKind::SessionStatus => {
            if let Some(status) = super::read!(envelope, SessionStatus) {
                figures.status = Some(status);
            }
        }
        EventKind::PreambleBuilt => {
            if let Some(built) = super::read!(envelope, PreambleBuilt) {
                figures.model = Some(built.model);
                figures.thinking = built.thinking;
                figures.credential = Some(built.credential);
                figures.window = Some(built.context_window);
                figures.trigger_at = built.trigger_at;
                // The latest build's budget is the one in force; a
                // build without one clears it.
                figures.budget = built.budget;
            }
        }
        EventKind::ModelChanged => {
            if let Some(changed) = super::read!(envelope, ModelChanged) {
                figures.model = Some(changed.after.model);
                figures.thinking = changed.after.thinking;
                figures.credential = Some(changed.after.credential);
            }
        }
        EventKind::TurnCompleted => {
            figures.turns = figures.turns.saturating_add(1);
            figures.started_at = None;
            // A settled turn ends any thinking it streamed: a
            // thinking-only turn never draws a stale `∴`.
            figures.thinking_state = ThinkingState::Closed;
        }
        EventKind::AssistantMessageStarted => {
            figures.started_at = Some(envelope.ts);
            // Every model call opens here, so the new call starts
            // with no thinking streamed yet.
            figures.thinking_state = ThinkingState::Idle;
        }
        // Only streamed thinking sets the flag: a bare start marker
        // also fires for replies with no streamed thought, and an
        // ephemeral delta can pass unseen, in which case no glyph is
        // promised (`docs/architecture.md`, "Streaming").
        EventKind::ReasoningDelta => {
            if figures.thinking_state != ThinkingState::Closed {
                figures.thinking_state = ThinkingState::Thinking;
            }
        }
        // The first text, the first tool call the model starts
        // emitting, or the call's end stops the thinking, whichever
        // comes first; later deltas of this call stay ignored.
        EventKind::AssistantMessageDelta
        | EventKind::ToolCallArgumentsDelta
        | EventKind::AssistantMessageCompleted => {
            figures.thinking_state = ThinkingState::Closed;
        }
        EventKind::UsageRecorded => {
            if let Some(usage) = super::read!(envelope, UsageRecorded)
                && usage.origin_session_id.is_none()
                && usage.extension.is_none()
            {
                figures.speed = figures
                    .started_at
                    .and_then(|started| envelope.ts.checked_sub(started))
                    .and_then(|span| {
                        usage
                            .tokens
                            .output
                            .checked_mul(1000)
                            .and_then(|scaled| scaled.checked_div(span))
                    });
            }
        }
        EventKind::McpServerFailed => {
            if let Some(failed) = super::read!(envelope, McpServerFailed) {
                figures.down.insert(failed.server);
            }
        }
        EventKind::McpServerReady => {
            if let Some(ready) = super::read!(envelope, McpServerReady) {
                figures.down.remove(&ready.server);
            }
        }
        EventKind::ExtensionUi
        | EventKind::SessionStarted
        | EventKind::ToolCallCompleted
        | EventKind::JobStarted
        | EventKind::DelegateStarted
        | EventKind::DelegateFinished
        | EventKind::JobCompleted
        | EventKind::FiberStarted
        | EventKind::FiberExited
        | EventKind::Rewound
        | EventKind::Moved
        | EventKind::TurnStarted
        | EventKind::StepStarted
        | EventKind::SteeringApplied
        | EventKind::SteeringQueue
        | EventKind::ShellCommand
        | EventKind::ShellCommandStarted
        | EventKind::ShellCommandDelta
        | EventKind::DirectoryAdded
        | EventKind::SessionNamed
        | EventKind::Archived
        | EventKind::Clients
        | EventKind::ContextAdded
        | EventKind::TextCompleted
        | EventKind::ReasoningStarted
        | EventKind::ReasoningCompleted
        | EventKind::ToolCallRequested
        | EventKind::ToolCallStarted
        | EventKind::ToolCallDelta
        | EventKind::PermissionRequested
        | EventKind::PermissionResolved
        | EventKind::InteractionRequested
        | EventKind::InteractionResolved
        | EventKind::RepositoryCodeOffered
        | EventKind::RepositoryCodeResolved
        | EventKind::QuotaNoticed
        | EventKind::QuotaRearmed
        | EventKind::RetryScheduled
        | EventKind::Notice
        | EventKind::OpeningMessage
        | EventKind::InstructionFile
        | EventKind::DateChanged
        | EventKind::SkillsChanged
        | EventKind::CompactionStarted
        | EventKind::CompactionCompleted
        | EventKind::SkillsResent
        | EventKind::ContextNudged
        | EventKind::ReviewerKept
        | EventKind::Reloaded
        | EventKind::ExtensionsLoaded
        | EventKind::ExtensionStateSet
        | EventKind::ExtensionStateUnset
        | EventKind::ExtensionMessage
        | EventKind::ExtensionLog
        | EventKind::ExtensionExec
        | EventKind::ExtensionHello
        | EventKind::ExtensionRequest
        | EventKind::JobDelta
        | EventKind::JobLine
        | EventKind::JobsPendingNotified
        | EventKind::CommandAccepted
        | EventKind::CommandRejected
        | EventKind::Unknown(_) => {}
    }
}

/// The open Fiber delegate's folded Session figures: its own
/// envelopes, folded while its view is open (`docs/tui.md`, "Swapped
/// views"). Every figure mirrors the attached fold's; nothing here
/// feeds the parent's cards.
#[derive(Debug, Default)]
struct DelegateFigures {
    /// The delegate session these figures came from: the views read
    /// them only for that session, so another delegate's leftovers never
    /// show as its own.
    session: Option<SessionId>,
    /// The delegate's session figures, through the shared fold.
    figures: SessionFigures,
}

/// The last readable figures retained when a delegate finishes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FinishedFigures {
    pub(crate) calls: u64,
    pub(crate) started: u64,
    pub(crate) spend: Option<(Option<f64>, f64)>,
}

/// The attached session's folded panel data.
#[derive(Debug, Default)]
pub(crate) struct PanelState {
    /// The attached session's quota answer and query state.
    pub(crate) quota: quota::Query,
    widgets: Vec<Widget>,
    /// The attached session's figures, through the shared fold.
    figures: SessionFigures,
    /// The session whose `session_status` supplied the figures' status.
    status_session: Option<SessionId>,
    /// The credential label this attachment's `start` named, until a
    /// `preamble_built` or `model_changed` replaces it.
    start_credential: Option<String>,
    /// Whether the attached session runs in a worktree Fiber created.
    worktree: bool,
    /// The attached session's recorded workspace root, from the latest
    /// `session_started`: `/pwd` shows it.
    workspace: Option<String>,
    changes: BTreeMap<String, (u64, u64)>,
    jobs: Vec<(JobId, String)>,
    delegates: BTreeMap<JobId, DelegateStarted>,
    /// How each finished delegate ended, by job: a `job_completed` for a
    /// delegate keeps its `jobs` entry and records its `status` here, so
    /// the card keeps showing it as done (`docs/tui.md`, "The panel").
    finished: BTreeMap<JobId, Outcome>,
    ended: BTreeMap<JobId, u64>,
    last: BTreeMap<JobId, FinishedFigures>,
    jobs_open: bool,
    due: bool,
    asked: Option<String>,
    again: bool,
    baseline: Option<bool>,
    branch: Option<Branch>,
    scroll: usize,
    delegate_scroll: usize,
    /// The open Fiber delegate's folded Session figures, keyed to its
    /// session; the attached fold never reads them.
    delegate: DelegateFigures,
    delegates_open: bool,
    /// The delegate sessions this attachment already tried to subscribe,
    /// so a refused subscribe goes out once per attachment
    /// (`docs/invocation.md`, `subscribe`).
    tried: BTreeSet<SessionId>,
}

impl PanelState {
    /// Folds one envelope of the attached session into the panel's data.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        match envelope.kind() {
            EventKind::ExtensionUi => self.fold_widget(envelope),
            EventKind::SessionStatus => {
                if let Some(status) = super::read!(envelope, SessionStatus) {
                    // The fold keeps the status with whose it is, so a
                    // previous session's status never supplies the new
                    // attachment's context (`docs/tui.md`, "Swapped
                    // views"); the delegate path shares the same store
                    // through the shared fold below.
                    self.figures.status = Some(status);
                    self.status_session = Some(envelope.session_id.clone());
                }
            }
            EventKind::PreambleBuilt
            | EventKind::ModelChanged
            | EventKind::TurnCompleted
            | EventKind::AssistantMessageStarted
            | EventKind::ReasoningDelta
            | EventKind::AssistantMessageDelta
            | EventKind::ToolCallArgumentsDelta
            | EventKind::AssistantMessageCompleted
            | EventKind::UsageRecorded
            | EventKind::McpServerFailed
            | EventKind::McpServerReady => {
                fold_session_figures(&mut self.figures, envelope);
            }
            EventKind::SessionStarted => {
                if let Some(started) = super::read!(envelope, SessionStarted) {
                    self.worktree = started.worktree.is_some();
                    self.workspace = Some(started.workspace.clone());
                }
            }
            EventKind::ToolCallCompleted => {
                if let Some(done) = super::read!(envelope, ToolCallCompleted) {
                    for change in done.changes.unwrap_or_default() {
                        let FileChange {
                            path,
                            added,
                            removed,
                        } = change;
                        let entry = self.changes.entry(path).or_default();
                        entry.0 = entry.0.saturating_add(added);
                        entry.1 = entry.1.saturating_add(removed);
                    }
                }
            }
            EventKind::JobStarted => {
                if let Some(started) = super::read!(envelope, JobStarted) {
                    // A resume is a new run of the same job: an id held
                    // from an earlier run goes last in start order, once
                    // (`docs/delegates.md`, "Identity and resume").
                    self.jobs.retain(|(id, _)| *id != started.job_id);
                    self.finished.remove(&started.job_id);
                    self.ended.remove(&started.job_id);
                    self.last.remove(&started.job_id);
                    self.jobs.push((started.job_id, started.description));
                }
            }
            EventKind::DelegateStarted => {
                if let Some(started) = super::read!(envelope, DelegateStarted) {
                    self.delegates.insert(started.job_id.clone(), started);
                }
            }
            EventKind::DelegateFinished => {
                if let Some(done) = super::read!(envelope, contract::events::DelegateFinished) {
                    self.ended.insert(done.job_id, envelope.ts);
                }
            }
            EventKind::JobCompleted => {
                if let Some(done) = super::read!(envelope, JobCompleted) {
                    // A finished delegate stays on its card as done; a
                    // plain job leaves the fold.
                    if self.delegates.contains_key(&done.job_id) {
                        self.ended.entry(done.job_id.clone()).or_insert(envelope.ts);
                        self.finished.insert(done.job_id, done.status);
                    } else {
                        self.jobs.retain(|(id, _)| *id != done.job_id);
                    }
                }
            }
            EventKind::FiberStarted
            | EventKind::FiberExited
            | EventKind::Rewound
            | EventKind::Moved
            | EventKind::TurnStarted
            | EventKind::StepStarted
            | EventKind::SteeringApplied
            | EventKind::SteeringQueue
            | EventKind::ShellCommand
            | EventKind::ShellCommandStarted
            | EventKind::ShellCommandDelta
            | EventKind::DirectoryAdded
            | EventKind::SessionNamed
            | EventKind::Archived
            | EventKind::Clients
            | EventKind::ContextAdded
            | EventKind::TextCompleted
            | EventKind::ReasoningStarted
            | EventKind::ReasoningCompleted
            | EventKind::ToolCallRequested
            | EventKind::ToolCallStarted
            | EventKind::ToolCallDelta
            | EventKind::PermissionRequested
            | EventKind::PermissionResolved
            | EventKind::InteractionRequested
            | EventKind::InteractionResolved
            | EventKind::RepositoryCodeOffered
            | EventKind::RepositoryCodeResolved
            | EventKind::QuotaNoticed
            | EventKind::QuotaRearmed
            | EventKind::RetryScheduled
            | EventKind::Notice
            | EventKind::OpeningMessage
            | EventKind::InstructionFile
            | EventKind::DateChanged
            | EventKind::SkillsChanged
            | EventKind::CompactionStarted
            | EventKind::CompactionCompleted
            | EventKind::SkillsResent
            | EventKind::ContextNudged
            | EventKind::ReviewerKept
            | EventKind::Reloaded
            | EventKind::ExtensionsLoaded
            | EventKind::ExtensionStateSet
            | EventKind::ExtensionStateUnset
            | EventKind::ExtensionMessage
            | EventKind::ExtensionLog
            | EventKind::ExtensionExec
            | EventKind::ExtensionHello
            | EventKind::ExtensionRequest
            | EventKind::JobDelta
            | EventKind::JobLine
            | EventKind::JobsPendingNotified
            | EventKind::CommandAccepted
            | EventKind::CommandRejected
            | EventKind::Unknown(_) => {}
        }
    }

    /// Folds an extension widget's latest lines: they replace it in place,
    /// empty lines remove it, and a new one goes last (`docs/tui.md`,
    /// "The panel"). A status line is not a widget.
    fn fold_widget(&mut self, envelope: &Envelope) {
        let Some(ui) = super::read!(envelope, ExtensionUi) else {
            return;
        };
        let Ui::Widget { widget, lines } = ui.ui else {
            return;
        };
        if lines.is_empty() {
            self.widgets
                .retain(|known| known.extension != ui.extension || known.widget != widget);
        } else if let Some(known) = self
            .widgets
            .iter_mut()
            .find(|known| known.extension == ui.extension && known.widget == widget)
        {
            known.lines = lines;
        } else {
            self.widgets.push(Widget {
                extension: ui.extension.clone(),
                widget,
                lines,
            });
        }
    }

    /// Folds one open Fiber delegate's envelope into the
    /// delegate-scoped figures. The caller routes only the open Fiber
    /// delegate's session here; any other session never reaches this
    /// fold, so its lines never touch these figures (`docs/tui.md`,
    /// "Swapped views").
    pub(crate) fn fold_delegate(&mut self, envelope: &Envelope) {
        // The fold is keyed to one delegate session: a new session starts
        // from unknown, never inheriting the previous session's cached
        // status, model, context or spend (`docs/tui.md`, "Swapped views").
        if self.delegate.session.as_ref() != Some(&envelope.session_id) {
            self.clear_delegate();
        }
        self.delegate.session = Some(envelope.session_id.clone());
        match envelope.kind() {
            EventKind::SessionStatus
            | EventKind::PreambleBuilt
            | EventKind::ModelChanged
            | EventKind::TurnCompleted
            | EventKind::AssistantMessageStarted
            | EventKind::ReasoningDelta
            | EventKind::AssistantMessageDelta
            | EventKind::ToolCallArgumentsDelta
            | EventKind::AssistantMessageCompleted
            | EventKind::UsageRecorded
            | EventKind::McpServerFailed
            | EventKind::McpServerReady => {
                // The attached fold shares this fold, fed its own
                // session; any other envelope leaves the figures alone.
                fold_session_figures(&mut self.delegate.figures, envelope);
            }
            EventKind::ExtensionUi
            | EventKind::SessionStarted
            | EventKind::ToolCallCompleted
            | EventKind::JobStarted
            | EventKind::DelegateStarted
            | EventKind::DelegateFinished
            | EventKind::JobCompleted
            | EventKind::FiberStarted
            | EventKind::FiberExited
            | EventKind::Rewound
            | EventKind::Moved
            | EventKind::TurnStarted
            | EventKind::StepStarted
            | EventKind::SteeringApplied
            | EventKind::SteeringQueue
            | EventKind::ShellCommand
            | EventKind::ShellCommandStarted
            | EventKind::ShellCommandDelta
            | EventKind::DirectoryAdded
            | EventKind::SessionNamed
            | EventKind::Archived
            | EventKind::Clients
            | EventKind::ContextAdded
            | EventKind::TextCompleted
            | EventKind::ReasoningStarted
            | EventKind::ReasoningCompleted
            | EventKind::ToolCallRequested
            | EventKind::ToolCallStarted
            | EventKind::ToolCallDelta
            | EventKind::PermissionRequested
            | EventKind::PermissionResolved
            | EventKind::InteractionRequested
            | EventKind::InteractionResolved
            | EventKind::RepositoryCodeOffered
            | EventKind::RepositoryCodeResolved
            | EventKind::QuotaNoticed
            | EventKind::QuotaRearmed
            | EventKind::RetryScheduled
            | EventKind::Notice
            | EventKind::OpeningMessage
            | EventKind::InstructionFile
            | EventKind::DateChanged
            | EventKind::SkillsChanged
            | EventKind::CompactionStarted
            | EventKind::CompactionCompleted
            | EventKind::SkillsResent
            | EventKind::ContextNudged
            | EventKind::ReviewerKept
            | EventKind::Reloaded
            | EventKind::ExtensionsLoaded
            | EventKind::ExtensionStateSet
            | EventKind::ExtensionStateUnset
            | EventKind::ExtensionMessage
            | EventKind::ExtensionLog
            | EventKind::ExtensionExec
            | EventKind::ExtensionHello
            | EventKind::ExtensionRequest
            | EventKind::JobDelta
            | EventKind::JobLine
            | EventKind::JobsPendingNotified
            | EventKind::CommandAccepted
            | EventKind::CommandRejected
            | EventKind::Unknown(_) => {}
        }
    }

    /// Drops the delegate-scoped figures: a resume of the open job
    /// starts from unknown, never from the previous run's figures
    /// (`docs/tui.md`, "Swapped views").
    fn clear_delegate(&mut self) {
        self.delegate = DelegateFigures::default();
    }

    /// The delegate fold when it holds `session`'s figures.
    fn delegate_for(&self, session: &SessionId) -> Option<&DelegateFigures> {
        (self.delegate.session.as_ref() == Some(session)).then_some(&self.delegate)
    }

    /// The delegate's cached `session_status`, only when it belongs to
    /// `session`.
    pub(crate) fn delegate_status_for(&self, session: &SessionId) -> Option<&SessionStatus> {
        self.delegate_for(session)
            .and_then(|delegate| delegate.figures.status.as_ref())
    }

    /// The model the delegate runs on: its latest `session_status`
    /// model first, else its latest `preamble_built` or `model_changed`
    /// model; `None` while neither has landed.
    pub(crate) fn delegate_model_for(&self, session: &SessionId) -> Option<&str> {
        self.delegate_for(session).and_then(|delegate| {
            delegate
                .figures
                .status
                .as_ref()
                .map(|status| status.model.as_str())
                .or(delegate.figures.model.as_deref())
        })
    }

    /// The thinking level the delegate's latest `preamble_built` or
    /// `model_changed` names.
    pub(crate) fn delegate_thinking_for(&self, session: &SessionId) -> Option<&str> {
        self.delegate_for(session)
            .and_then(|delegate| delegate.figures.thinking.as_deref())
    }

    /// Whether the delegate's running model call is streaming thinking
    /// now.
    pub(crate) fn delegate_thinking_now_for(&self, session: &SessionId) -> bool {
        self.delegate_for(session)
            .is_some_and(|delegate| delegate.figures.thinking_state == ThinkingState::Thinking)
    }

    /// The context window in force for the delegate: its latest own
    /// `preamble_built`'s, or its reported window when it logged no own
    /// preamble.
    pub(crate) fn delegate_window_for(&self, session: &SessionId) -> Option<u64> {
        self.delegate_for(session).and_then(|delegate| {
            delegate.figures.window.or_else(|| {
                delegate
                    .figures
                    .status
                    .as_ref()?
                    .context
                    .as_ref()
                    .map(|context| context.window)
            })
        })
    }

    /// The delegate's latest `preamble_built`'s compaction point, in
    /// tokens.
    pub(crate) fn delegate_trigger_for(&self, session: &SessionId) -> Option<u64> {
        self.delegate_for(session)
            .and_then(|delegate| delegate.figures.trigger_at)
    }

    /// The delegate's latest `preamble_built`'s budget, in US dollars;
    /// `None` when it set none.
    pub(crate) fn delegate_budget_for(&self, session: &SessionId) -> Option<f64> {
        self.delegate_for(session)
            .and_then(|delegate| delegate.figures.budget)
    }

    /// How many of the delegate's turns have completed.
    pub(crate) fn delegate_turns_for(&self, session: &SessionId) -> u64 {
        self.delegate_for(session)
            .map(|delegate| delegate.figures.turns)
            .unwrap_or(0)
    }

    /// The delegate's last reply's output speed, in tokens per second.
    pub(crate) fn delegate_speed_for(&self, session: &SessionId) -> Option<u64> {
        self.delegate_for(session)
            .and_then(|delegate| delegate.figures.speed)
    }

    /// The MCP servers down on the delegate, by name; `None` when none
    /// are down or no delegate figures are held.
    pub(crate) fn delegate_down_for(&self, session: &SessionId) -> Option<&BTreeSet<String>> {
        self.delegate_for(session)
            .filter(|delegate| !delegate.figures.down.is_empty())
            .map(|delegate| &delegate.figures.down)
    }

    /// The MCP servers down the Session card names while `session`'s
    /// delegate view is open: its own set, empty when none are down,
    /// never the attached session's (`docs/tui.md`, "Swapped views").
    pub(crate) fn delegate_tools_for(&self, session: &SessionId) -> &BTreeSet<String> {
        static EMPTY: std::sync::LazyLock<BTreeSet<String>> =
            std::sync::LazyLock::new(BTreeSet::new);
        self.delegate_for(session)
            .map(|delegate| &delegate.figures.down)
            .unwrap_or(&EMPTY)
    }

    /// Everything back to default.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The attached session's widgets, in arrival order.
    pub(crate) fn widgets(&self) -> &[Widget] {
        &self.widgets
    }

    /// The latest `session_status` of the attached session, if any.
    pub(crate) fn status(&self) -> Option<&SessionStatus> {
        self.figures.status.as_ref()
    }

    /// The model the session runs on: the latest `session_status`
    /// model first, else the latest `preamble_built` or
    /// `model_changed` model; `None` while neither has landed, so no
    /// display falls back to the configured default
    /// (`docs/tui.md`, "The input box").
    pub(crate) fn session_model(&self) -> Option<&str> {
        self.figures
            .status
            .as_ref()
            .map(|status| status.model.as_str())
            .or(self.figures.model.as_deref())
    }

    /// The thinking level the latest `preamble_built` or `model_changed`
    /// names.
    pub(crate) fn thinking(&self) -> Option<&str> {
        self.figures.thinking.as_deref()
    }

    /// Whether the running model call is streaming thinking now,
    /// whatever level the latest preamble names: the Session card draws
    /// its `∴` only when a level and this flag meet
    /// (`docs/tui.md`, "The panel").
    pub(crate) fn thinking_now(&self) -> bool {
        self.figures.thinking_state == ThinkingState::Thinking
    }

    /// The credential label the latest `preamble_built` or
    /// `model_changed` names: `None` before either, `Some(None)` when
    /// the provider takes no credential.
    pub(crate) fn credential(&self) -> Option<Option<&str>> {
        self.figures.credential.as_ref().map(Option::as_deref)
    }

    /// Records the credential label a `start`'s `args` name, so a later
    /// session keeps it until its own build or switch names one. Every
    /// `start` records, since each attachment supersedes the last.
    pub(crate) fn note_start(&mut self, args: &Value) {
        self.start_credential = start_credential_label(args);
    }

    /// Carries the effective credential state into the cleared session's
    /// fresh panel, after the reset the attach starts from.
    pub(crate) fn carry_clear_credential(&mut self, credential: Option<Option<String>>) {
        self.start_credential = credential.clone().flatten();
        self.figures.credential = credential;
    }

    /// The credential state a clear carries: the latest build or switch,
    /// including an explicit absence, otherwise the label its `start` named.
    pub(crate) fn clear_credential(&self) -> Option<Option<&str>> {
        self.figures
            .credential
            .as_ref()
            .map(Option::as_deref)
            .or_else(|| self.start_credential.as_deref().map(Some))
    }

    /// Whether the attached session runs in a worktree Fiber created.
    pub(crate) fn worktree(&self) -> bool {
        self.worktree
    }

    /// The attached session's recorded workspace root, from the latest
    /// `session_started` (`docs/tui.md`, "Slash commands"): `/pwd` shows
    /// it. `None` before the first fold.
    pub(crate) fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// The context window in force: the latest own `preamble_built`'s,
    /// or the active session's reported window when it logged no own
    /// preamble (`docs/tui.md`, "Swapped views").
    pub(crate) fn window_for(&self, session: &SessionId) -> Option<u64> {
        self.figures.window.or_else(|| {
            self.status_for(session)?
                .context
                .as_ref()
                .map(|context| context.window)
        })
    }

    /// The cached status only when it belongs to `session`.
    pub(crate) fn status_for(&self, session: &SessionId) -> Option<&SessionStatus> {
        if self.status_session.as_ref() == Some(session) {
            self.figures.status.as_ref()
        } else {
            None
        }
    }

    /// The latest `preamble_built`'s compaction point, in tokens.
    pub(crate) fn trigger_at(&self) -> Option<u64> {
        self.figures.trigger_at
    }

    /// The automatic compaction point in force: the latest own
    /// `preamble_built`'s, known off when that build sets none, or
    /// unknown when the session logged no `preamble_built` of its own.
    /// A build always sets the window, so a window means an own build
    /// replaced the inherited one (`docs/tui.md`, "Swapped views").
    pub(crate) fn compaction(&self) -> Compaction {
        match (self.figures.window.is_some(), self.figures.trigger_at) {
            (true, Some(point)) => Compaction::At(point),
            (true, None) => Compaction::Off,
            (false, _) => Compaction::Unknown,
        }
    }

    /// The latest `preamble_built`'s budget, in US dollars; `None` when
    /// it set none.
    pub(crate) fn budget(&self) -> Option<f64> {
        self.figures.budget
    }

    /// How many turns have completed.
    pub(crate) fn turns(&self) -> u64 {
        self.figures.turns
    }

    /// The last reply's output speed, in tokens per second.
    pub(crate) fn speed(&self) -> Option<u64> {
        self.figures.speed
    }

    /// The MCP servers down, by name.
    pub(crate) fn down(&self) -> &BTreeSet<String> {
        &self.figures.down
    }

    /// The lines changed per path: added and removed.
    pub(crate) fn changes(&self) -> &BTreeMap<String, (u64, u64)> {
        &self.changes
    }

    /// The jobs started in start order: their ids and descriptions.
    /// Only tests read the whole fold; the cards read `running_jobs` and
    /// `running_delegates`.
    #[cfg(test)]
    pub(crate) fn jobs(&self) -> &[(JobId, String)] {
        &self.jobs
    }

    /// The delegates' jobs: a delegate is a job with a `delegate_started`
    /// (`docs/events.md`, "`delegate_started`").
    pub(crate) fn delegate_jobs(&self) -> &BTreeMap<JobId, DelegateStarted> {
        &self.delegates
    }

    /// Running delegates in job start order, each with its job's
    /// description: a job with a `delegate_started` and no `job_completed`
    /// yet (`docs/events.md`, "`delegate_started`").
    pub(crate) fn running_delegates(&self) -> Vec<(&DelegateStarted, &str)> {
        self.jobs
            .iter()
            .filter(|(id, _)| !self.finished.contains_key(id))
            .filter_map(|(id, description)| {
                self.delegates
                    .get(id)
                    .map(|started| (started, description.as_str()))
            })
            .collect()
    }

    /// Keeps the last readable summary before the delegate's row is dropped.
    pub(super) fn keep_last(
        &mut self,
        job: JobId,
        calls: u64,
        started: u64,
        spend: Option<(Option<f64>, f64)>,
    ) {
        self.last.insert(
            job,
            FinishedFigures {
                calls,
                started,
                spend,
            },
        );
    }

    /// The finished delegate's calls and elapsed milliseconds, when known.
    pub(crate) fn finished_figures(&self, job: &JobId) -> Option<(u64, u64)> {
        let last = self.last.get(job)?;
        let ended = self.ended.get(job)?;
        Some((last.calls, ended.saturating_sub(last.started)))
    }

    /// The finished delegate's last reported billed and subscription totals.
    pub(crate) fn finished_cost(&self, job: &JobId) -> Option<(Option<f64>, f64)> {
        self.last.get(job)?.spend
    }

    /// Finished delegates in job start order, each with its job's
    /// description and how it ended: a job with a `delegate_started`
    /// and a `job_completed` (`docs/tui.md`, "The panel").
    pub(crate) fn finished_delegates(&self) -> Vec<(&DelegateStarted, &str, Outcome)> {
        self.jobs
            .iter()
            .filter_map(|(id, description)| {
                self.finished.get(id).and_then(|outcome| {
                    self.delegates
                        .get(id)
                        .map(|started| (started, description.as_str(), *outcome))
                })
            })
            .collect()
    }

    /// Running non-delegate jobs in job start order: a job with a
    /// `delegate_started` shows on the Delegates card instead (`docs/tui.md`,
    /// "The panel").
    pub(crate) fn running_jobs(&self) -> Vec<(&JobId, &str)> {
        self.jobs
            .iter()
            .filter(|(id, _)| !self.delegates.contains_key(id))
            .map(|(id, description)| (id, description.as_str()))
            .collect()
    }

    /// Whether the Jobs card lists its jobs.
    pub(crate) fn jobs_open(&self) -> bool {
        self.jobs_open
    }

    /// A branch query is due on the next attached-session line, and the
    /// old session's recorded root ends here: the new attachment folds
    /// its own `session_started`, so `/pwd` waits for that fold instead
    /// of showing stale state (`docs/tui.md`, "Slash commands").
    pub(crate) fn attached(&mut self) {
        self.due = true;
        self.quota.attached();
        self.workspace = None;
        // The new attachment folds its own `preamble_built` and
        // `session_status`: the old window and compaction point end here.
        // Keep the cached status; its session identity prevents it from
        // supplying the new attachment's context (`docs/tui.md`, "Swapped views").
        self.figures.window = None;
        self.figures.trigger_at = None;
        // A new attachment has no running call, so no streamed thinking.
        self.figures.thinking_state = ThinkingState::Idle;
        self.delegate_scroll = 0;
        self.delegates_open = false;
    }

    /// The branch the last query answered, if any.
    pub(crate) fn branch(&self) -> Option<&Branch> {
        self.branch.as_ref()
    }

    /// How many rows the panel has scrolled.
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// How many entries the Delegates card has scrolled.
    pub(crate) fn delegate_scroll(&self) -> usize {
        self.delegate_scroll
    }

    /// Whether the finished delegates are expanded.
    pub(crate) fn delegates_open(&self) -> bool {
        self.delegates_open
    }

    /// Toggles the finished delegates, preserving and clamping the offset.
    pub(super) fn toggle_delegates(&mut self) {
        self.delegates_open = !self.delegates_open;
        self.clamp_delegate_scroll();
    }

    /// Records `session` as tried on this attachment; true the first time.
    /// A refused subscribe is not sent again until the panel resets
    /// (`docs/invocation.md`, `subscribe`).
    pub(super) fn try_delegate(&mut self, session: &SessionId) -> bool {
        self.tried.insert(session.clone())
    }

    /// Moves one whole delegate or fold entry in the six-row window.
    pub(super) fn scroll_delegates(&mut self, up: bool) {
        self.clamp_delegate_scroll();
        let max = self.delegate_scroll_max();
        self.delegate_scroll = if up {
            self.delegate_scroll.saturating_sub(1)
        } else {
            self.delegate_scroll.saturating_add(1).min(max)
        };
    }

    pub(super) fn clamp_delegate_scroll(&mut self) {
        self.delegate_scroll = self.delegate_scroll.min(self.delegate_scroll_max());
    }

    fn delegate_scroll_max(&self) -> usize {
        let finished = self.finished_delegates().len();
        let entries = self.running_delegates().len()
            + usize::from(finished > 0)
            + if self.delegates_open { finished } else { 0 };
        let weight = |at: usize| {
            usize::from(at != self.running_delegates().len()) * 2
                + usize::from(at == self.running_delegates().len())
        };
        let mut total = 0;
        for at in (0..entries).rev() {
            total += weight(at);
            if total > 6 {
                return at + 1;
            }
        }
        0
    }

    /// Folds the branch query's answer: the first line trimmed is the
    /// branch, `HEAD` reads detached, and anything else leaves the row
    /// out. Clears the query; a turn end held while it was in flight
    /// sends one more. Returns whether another query goes out now.
    fn answer_branch(&mut self, result: Option<&CommandResult>) -> bool {
        let branch = match result {
            Some(CommandResult::Shell {
                output, process, ..
            }) if process.signal.is_none() && process.exit_code == Some(0) => {
                match output.lines().next().map(str::trim) {
                    Some("HEAD") => Branch::Detached,
                    Some(name) if !name.is_empty() => Branch::Named(name.to_owned()),
                    _ => Branch::Absent,
                }
            }
            _ => Branch::Absent,
        };
        self.branch = Some(branch);
        self.asked = None;
        std::mem::replace(&mut self.again, false)
    }

    /// Whether the panel still holds the branch query `id`: the
    /// test-only order seam reads it, so a test pins the generic
    /// refusal settling before the panel's clears it.
    #[cfg(test)]
    pub(super) fn asked_is(&self, id: &str) -> bool {
        self.asked.as_deref() == Some(id)
    }

    /// A rejection of the branch query leaves the row out, with no notice:
    /// it behaves as a typed command's rejection does, minus the notice a
    /// person typed for. Returns whether another query goes out now.
    fn refuse_branch(&mut self) -> bool {
        self.branch = Some(Branch::Absent);
        self.asked = None;
        std::mem::replace(&mut self.again, false)
    }

    /// Folds a `session_status` into the turn baseline: the first status
    /// after attach only sets it; a later settled status after a busy one
    /// ends a turn. Returns whether this status ends a turn.
    fn turn_end(&mut self, busy: bool) -> bool {
        let end = self.baseline == Some(true) && !busy;
        self.baseline = Some(busy);
        end
    }
}

impl App {
    /// The side panel's folded state.
    pub(crate) fn panel_state(&self) -> &PanelState {
        &self.panel_state
    }

    /// The open Fiber delegate's session, while its view is open: the
    /// Session figures follow it. A job's view, another harness's
    /// delegate, or no view reads the parent's (`docs/tui.md`, "Swapped
    /// views").
    pub(crate) fn delegate_session(&self) -> Option<SessionId> {
        let view = self.item_view()?;
        if view.harness != "fiber" {
            return None;
        }
        view.session_label.map(SessionId)
    }

    /// The card list the panel draws, in order; empty without home, so no
    /// card draws there.
    pub(crate) fn panel_cards(&self) -> &[String] {
        self.home.launch.panel_cards.as_slice()
    }

    /// Folds one attached-session envelope into the panel's data, with the
    /// Delegates card's subscribes (`docs/tui.md`, "The panel"). The
    /// branch and quota answers route by tag; every other line folds as
    /// before.
    pub(super) fn panel_line(
        &mut self,
        envelope: &Envelope,
        answer: Option<&super::answer::Answer<'_>>,
    ) -> Vec<String> {
        self.panel_state.fold(envelope);
        // A resume of the open item's job clears the delegate overlay:
        // the new run folds its own figures, never inheriting the
        // previous run's. Any other job's resume leaves the open view's
        // figures intact (`docs/tui.md`, "Swapped views").
        if envelope.kind() == EventKind::JobStarted
            && let Some(started) = super::read!(envelope, contract::events::JobStarted)
            && self
                .item_view()
                .is_some_and(|view| view.job_id == started.job_id)
        {
            self.panel_state.clear_delegate();
        }
        let ended = if envelope.kind() == EventKind::SessionStatus {
            super::read!(envelope, SessionStatus).is_some_and(|status| {
                let busy = !matches!(status.state, SessionState::Idle | SessionState::Jobs);
                self.panel_state.turn_end(busy)
            })
        } else {
            false
        };
        match answer.and_then(|answer| answer.waiting.as_ref()) {
            Some(super::Waiting::Branch) => {
                if let Some(answer) = answer {
                    self.outbox.settled(answer.id);
                }
                let mut send = self.panel_branch(envelope, ended);
                send.extend(self.delegate_line(envelope));
                return send;
            }
            Some(super::Waiting::Quota) => {
                if let Some(answer) = answer {
                    self.outbox.settled(answer.id);
                }
                let mut send = Vec::new();
                send.extend(self.panel_quota(envelope, ended));
                send.extend(self.delegate_line(envelope));
                return send;
            }
            Some(
                super::Waiting::Find
                | super::Waiting::Commands
                | super::Waiting::Model
                | super::Waiting::Reply
                | super::Waiting::OfferFile
                | super::Waiting::Steer
                | super::Waiting::Shell,
            ) => {}
            Some(
                super::Waiting::Dismiss(_)
                | super::Waiting::Fork
                | super::Waiting::RewindHistory
                | super::Waiting::RewindSend
                | super::Waiting::ChangeWorkspace(_)
                | super::Waiting::ClearStart
                | super::Waiting::ClearClose
                | super::Waiting::History
                | super::Waiting::ImageFetch { .. }
                | super::Waiting::ImageDisplay { .. }
                | super::Waiting::Sessions
                | super::Waiting::Abandoned
                | super::Waiting::Directories(..)
                | super::Waiting::Subscribe { .. }
                | super::Waiting::Stop
                | super::Waiting::Delete
                | super::Waiting::OfferImport
                | super::Waiting::DeleteDiscovery(..)
                | super::Waiting::DeleteStop { .. }
                | super::Waiting::DeleteFinal(..)
                | super::Waiting::Feed
                | super::Waiting::Recent(..)
                | super::Waiting::Close,
            )
            | Some(
                super::Waiting::Figures
                | super::Waiting::Diff
                | super::Waiting::Hooks
                | super::Waiting::ArtifactPage
                | super::Waiting::ConfigAsk
                | super::Waiting::Credentials
                | super::Waiting::CredentialSwitch
                | super::Waiting::JobStop
                | super::Waiting::JobInput,
            )
            | None => {}
        }
        let mut send = self.panel_branch(envelope, ended);
        send.extend(self.panel_quota(envelope, ended));
        send.extend(self.delegate_line(envelope));
        send
    }

    /// The branch query's answers and triggers (`docs/tui.md`, "Git"): a
    /// `shell` running `git rev-parse --abbrev-ref HEAD` on attach and
    /// after each turn. Nothing polls.
    fn panel_branch(&mut self, envelope: &Envelope, ended: bool) -> Vec<String> {
        let mut send = Vec::new();
        match envelope.kind() {
            EventKind::CommandAccepted => {
                if let Some(accepted) = super::read!(envelope, CommandAccepted)
                    && self.panel_state.asked.as_deref() == Some(accepted.command_id.0.as_str())
                    && self.panel_state.answer_branch(accepted.result.as_ref())
                {
                    send.extend(self.branch_query());
                }
            }
            EventKind::CommandRejected => {
                if let Some(rejected) = super::read!(envelope, CommandRejected)
                    && rejected
                        .command_id
                        .as_ref()
                        .is_some_and(|id| self.panel_state.asked.as_deref() == Some(id.0.as_str()))
                    && self.panel_state.refuse_branch()
                {
                    send.extend(self.branch_query());
                }
            }
            EventKind::SessionStatus if ended => {
                if self.panel_state.asked.is_some() {
                    self.panel_state.again = true;
                } else {
                    send.extend(self.branch_query());
                }
            }
            EventKind::FiberStarted
            | EventKind::FiberExited
            | EventKind::SessionStarted
            | EventKind::Rewound
            | EventKind::Moved
            | EventKind::TurnStarted
            | EventKind::StepStarted
            | EventKind::TurnCompleted
            | EventKind::SteeringApplied
            | EventKind::SteeringQueue
            | EventKind::ShellCommand
            | EventKind::ShellCommandStarted
            | EventKind::ShellCommandDelta
            | EventKind::DirectoryAdded
            | EventKind::SessionNamed
            | EventKind::Archived
            | EventKind::Clients
            | EventKind::SessionStatus
            | EventKind::ContextAdded
            | EventKind::AssistantMessageStarted
            | EventKind::AssistantMessageDelta
            | EventKind::AssistantMessageCompleted
            | EventKind::TextCompleted
            | EventKind::ToolCallArgumentsDelta
            | EventKind::ReasoningStarted
            | EventKind::ReasoningDelta
            | EventKind::ReasoningCompleted
            | EventKind::ToolCallRequested
            | EventKind::ToolCallStarted
            | EventKind::ToolCallDelta
            | EventKind::ToolCallCompleted
            | EventKind::PermissionRequested
            | EventKind::PermissionResolved
            | EventKind::InteractionRequested
            | EventKind::InteractionResolved
            | EventKind::RepositoryCodeOffered
            | EventKind::RepositoryCodeResolved
            | EventKind::UsageRecorded
            | EventKind::QuotaNoticed
            | EventKind::QuotaRearmed
            | EventKind::RetryScheduled
            | EventKind::Notice
            | EventKind::PreambleBuilt
            | EventKind::ModelChanged
            | EventKind::OpeningMessage
            | EventKind::InstructionFile
            | EventKind::DateChanged
            | EventKind::SkillsChanged
            | EventKind::CompactionStarted
            | EventKind::CompactionCompleted
            | EventKind::SkillsResent
            | EventKind::ContextNudged
            | EventKind::ReviewerKept
            | EventKind::McpServerFailed
            | EventKind::McpServerReady
            | EventKind::Reloaded
            | EventKind::ExtensionsLoaded
            | EventKind::ExtensionStateSet
            | EventKind::ExtensionStateUnset
            | EventKind::ExtensionUi
            | EventKind::ExtensionMessage
            | EventKind::ExtensionLog
            | EventKind::ExtensionExec
            | EventKind::ExtensionHello
            | EventKind::ExtensionRequest
            | EventKind::JobStarted
            | EventKind::DelegateStarted
            | EventKind::JobDelta
            | EventKind::JobLine
            | EventKind::DelegateFinished
            | EventKind::JobCompleted
            | EventKind::JobsPendingNotified
            | EventKind::Unknown(_) => {}
        }
        if self.panel_state.due {
            send.extend(self.branch_query());
        }
        send
    }

    /// The branch query, when one goes out: due or a turn end, only the
    /// Session card shows a branch, the link is up, and the attached row
    /// has not left. Sending clears `due` and records the query; when a
    /// condition fails `due` stays set and the next line tries again. The
    /// query never enters `pending`, so its answer adds no conversation
    /// item and a rejection adds no notice.
    fn branch_query(&mut self) -> Option<String> {
        if !self.panel_cards().iter().any(|card| card == "session") {
            return None;
        }
        if !self.outbox.is_up() {
            return None;
        }
        if self.attached_row().is_some_and(|row| row.left.is_some()) {
            return None;
        }
        if self.panel_state.asked.is_some() {
            return None;
        }
        let session = self.session()?.clone();
        let id = mint();
        let line =
            shell::command(&id, &session, "git rev-parse --abbrev-ref HEAD", false).to_string();
        self.panel_state.due = false;
        self.outbox.wait(&id, super::Waiting::Branch);
        self.panel_state.asked = Some(id);
        Some(line)
    }

    /// A hub `command_rejected` for the branch query leaves the row out,
    /// with no notice, and clears the query so the next turn end sends.
    pub(super) fn panel_refused(&mut self, id: &str) -> Vec<String> {
        self.panel_state.quota.refuse(id);
        if self.panel_state.asked.as_deref() == Some(id) && self.panel_state.refuse_branch() {
            return self.branch_query().into_iter().collect();
        }
        Vec::new()
    }

    /// Scrolls the panel by three rows, picked, not measured: the stored
    /// offset clamps to the rows past the panel's text rows first, so a
    /// grown screen or shrunk cards move on the first wheel; down clamps
    /// to that end, up saturates at the top.
    pub(super) fn scroll_panel(&mut self, up: bool) {
        let Some(panel) = self.chrome().layout().and_then(|layout| layout.panel) else {
            return;
        };
        let rows = crate::view::panel::rows(self, panel.width);
        let height = usize::from(panel.height.saturating_sub(1));
        let max = rows.len().saturating_sub(height);
        let clamped = self.panel_state.scroll.min(max);
        if up {
            self.panel_state.scroll = clamped.saturating_sub(super::mouse::WHEEL_ROWS);
        } else {
            self.panel_state.scroll = clamped.saturating_add(super::mouse::WHEEL_ROWS).min(max);
        }
    }

    /// A click on a panel item.
    pub(super) fn panel_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Jobs => {
                self.panel_state.jobs_open = !self.panel_state.jobs_open;
                Effect::None
            }
            Spot::Branch => {
                let Some((session, _)) = self.command_session() else {
                    return Effect::None;
                };
                let id = mint();
                let line = shell::command(&id, &session, "git --no-optional-locks status", false)
                    .to_string();
                let mut draft = Draft::default();
                draft.set("!!git --no-optional-locks status");
                self.outbox.pending.insert(id, (Kind::Shell, draft));
                Effect::Send(vec![line])
            }
            Spot::Waiting => self.show_rail(),
            Spot::Model => self.open_model_picker(crate::model_picker::Mode::Choose),
            Spot::Context => self.open_session_view(super::SessionView::Context),
            Spot::Status => self.open_session_view(super::SessionView::Status),
            Spot::Usage => self.open_session_view(super::SessionView::Usage),
            Spot::Quota => self.quota_view(),
            Spot::Tools => self.open_config_view(super::ConfigView::Tools),
            Spot::File(rank) => self.open_changed_file(rank),
            Spot::DelegateFold => {
                self.panel_state.toggle_delegates();
                Effect::None
            }
            Spot::Delegate(serial) => self.open_serial(serial),
            Spot::Job(serial) => self.open_serial(serial),
            Spot::DelegateList => self.open_session_view(super::SessionView::Delegates),
            Spot::JobList => self.open_session_view(super::SessionView::Jobs),
            Spot::ChangedFiles => self.open_session_view(super::SessionView::ChangedFiles),
            Spot::Widget => self.toggle_widget_row(),
        }
    }
}

/// The credential label a `start`'s `args` name, if any: the value of its
/// `*.credential` override.
fn start_credential_label(args: &Value) -> Option<String> {
    args.get("overrides")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|entry| {
            let (key, label) = entry.split_once('=')?;
            key.strip_suffix(".credential")
                .is_some()
                .then(|| label.to_owned())
        })
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
