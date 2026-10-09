//! The session screen's side panel state: the attached session's folded
//! data behind a small interface (`docs/tui.md`, "The panel"). The app
//! calls [`App::panel_line`] for every attached-session line and reads the
//! fold through [`PanelState`]'s accessors; drawing lives in
//! `crate::view::panel`.

use std::collections::{BTreeMap, BTreeSet};

use contract::Envelope;
use contract::JobId;
use contract::SessionId;
use contract::events::{
    CommandAccepted, CommandRejected, CommandResult, DelegateStarted, ExtensionUi, FileChange,
    JobCompleted, JobStarted, McpServerFailed, McpServerReady, ModelChanged, PreambleBuilt,
    SessionState, SessionStatus, ToolCallCompleted, Ui, UsageRecorded,
};

use super::{App, Effect, Kind, Link, mint};
use crate::input::Draft;
use crate::shell;
use crate::view::panel::delegates::DELEGATES_SHOWN;

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

/// The attached session's folded panel data.
#[derive(Debug, Default)]
pub(crate) struct PanelState {
    widgets: Vec<Widget>,
    status: Option<SessionStatus>,
    model: Option<String>,
    thinking: Option<String>,
    window: Option<u64>,
    trigger_at: Option<u64>,
    turns: u64,
    started_at: Option<u64>,
    speed: Option<u64>,
    down: BTreeSet<String>,
    changes: BTreeMap<String, (u64, u64)>,
    jobs: Vec<(JobId, String)>,
    delegates: BTreeMap<JobId, DelegateStarted>,
    jobs_open: bool,
    due: bool,
    asked: Option<String>,
    again: bool,
    baseline: Option<bool>,
    branch: Option<Branch>,
    scroll: usize,
    delegate_scroll: usize,
    /// The delegate sessions this attachment already tried to subscribe,
    /// so a refused subscribe goes out once per attachment
    /// (`docs/invocation.md`, `subscribe`).
    tried: BTreeSet<SessionId>,
}

impl PanelState {
    /// Folds one envelope of the attached session into the panel's data.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        match envelope.kind.as_str() {
            "extension_ui" => self.fold_widget(envelope),
            "session_status" => {
                if let Some(status) = super::read!(envelope, SessionStatus) {
                    self.status = Some(status);
                }
            }
            "preamble_built" => {
                if let Some(built) = super::read!(envelope, PreambleBuilt) {
                    self.model = Some(built.model);
                    self.thinking = built.thinking;
                    self.window = Some(built.context_window);
                    self.trigger_at = built.trigger_at;
                }
            }
            "model_changed" => {
                if let Some(changed) = super::read!(envelope, ModelChanged) {
                    self.model = Some(changed.after.model);
                    self.thinking = changed.after.thinking;
                }
            }
            "turn_started" => {
                self.turns = self.turns.saturating_add(1);
                self.started_at = None;
            }
            "assistant_message_started" => {
                self.started_at = Some(envelope.ts);
            }
            "usage_recorded" => {
                if let Some(usage) = super::read!(envelope, UsageRecorded)
                    && usage.origin_session_id.is_none()
                    && usage.extension.is_none()
                {
                    self.speed = self
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
            "mcp_server_failed" => {
                if let Some(failed) = super::read!(envelope, McpServerFailed) {
                    self.down.insert(failed.server);
                }
            }
            "mcp_server_ready" => {
                if let Some(ready) = super::read!(envelope, McpServerReady) {
                    self.down.remove(&ready.server);
                }
            }
            "tool_call_completed" => {
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
            "job_started" => {
                if let Some(started) = super::read!(envelope, JobStarted) {
                    self.jobs.push((started.job_id, started.description));
                }
            }
            "delegate_started" => {
                if let Some(started) = super::read!(envelope, DelegateStarted) {
                    self.delegates.insert(started.job_id.clone(), started);
                }
            }
            "job_completed" => {
                if let Some(done) = super::read!(envelope, JobCompleted) {
                    self.jobs.retain(|(id, _)| *id != done.job_id);
                }
            }
            _ => {}
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
        self.status.as_ref()
    }

    /// The model the latest `preamble_built` or `model_changed` names.
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The thinking level the latest `preamble_built` or `model_changed`
    /// names.
    pub(crate) fn thinking(&self) -> Option<&str> {
        self.thinking.as_deref()
    }

    /// The latest `preamble_built`'s context window, in tokens.
    pub(crate) fn window(&self) -> Option<u64> {
        self.window
    }

    /// The latest `preamble_built`'s handoff point, in tokens.
    pub(crate) fn trigger_at(&self) -> Option<u64> {
        self.trigger_at
    }

    /// How many turns have started.
    pub(crate) fn turns(&self) -> u64 {
        self.turns
    }

    /// The last reply's output speed, in tokens per second.
    pub(crate) fn speed(&self) -> Option<u64> {
        self.speed
    }

    /// The MCP servers down, by name.
    pub(crate) fn down(&self) -> &BTreeSet<String> {
        &self.down
    }

    /// The lines changed per path: added and removed.
    pub(crate) fn changes(&self) -> &BTreeMap<String, (u64, u64)> {
        &self.changes
    }

    /// The jobs started in start order: their ids and descriptions.
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
            .filter_map(|(id, description)| {
                self.delegates
                    .get(id)
                    .map(|started| (started, description.as_str()))
            })
            .collect()
    }

    /// Whether the Jobs card lists its jobs.
    pub(crate) fn jobs_open(&self) -> bool {
        self.jobs_open
    }

    /// A branch query is due on the next attached-session line.
    pub(crate) fn attached(&mut self) {
        self.due = true;
    }

    /// The branch the last query answered, if any.
    pub(crate) fn branch(&self) -> Option<&Branch> {
        self.branch.as_ref()
    }

    /// How many rows the panel has scrolled.
    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    /// How many delegates the Delegates card has scrolled.
    pub(crate) fn delegate_scroll(&self) -> usize {
        self.delegate_scroll
    }

    /// Records `session` as tried on this attachment; true the first time.
    /// A refused subscribe is not sent again until the panel resets
    /// (`docs/invocation.md`, `subscribe`).
    pub(super) fn try_delegate(&mut self, session: &SessionId) -> bool {
        self.tried.insert(session.clone())
    }

    /// Clamps the Delegates card's offset to the running delegates less
    /// [`DELEGATES_SHOWN`], then moves it one delegate: down to the clamp,
    /// up saturating at 0 (`docs/tui.md`, "The panel").
    pub(super) fn scroll_delegates(&mut self, up: bool) {
        let max = self
            .running_delegates()
            .len()
            .saturating_sub(DELEGATES_SHOWN);
        let clamped = self.delegate_scroll.min(max);
        self.delegate_scroll = if up {
            clamped.saturating_sub(1)
        } else {
            clamped.saturating_add(1).min(max)
        };
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

    /// The card list the panel draws, in order; empty without home, so no
    /// card draws there.
    pub(crate) fn panel_cards(&self) -> &[String] {
        self.home
            .as_ref()
            .map(|home| home.launch.panel_cards.as_slice())
            .unwrap_or(&[])
    }

    /// Folds one attached-session envelope into the panel's data, with the
    /// Delegates card's subscribes (`docs/tui.md`, "The panel").
    pub(super) fn panel_line(&mut self, envelope: &Envelope) -> Vec<String> {
        self.panel_state.fold(envelope);
        let mut send = self.panel_branch(envelope);
        send.extend(self.delegate_line(envelope));
        send
    }

    /// The branch query's answers and triggers (`docs/tui.md`, "Git"): a
    /// `shell` running `git rev-parse --abbrev-ref HEAD` on attach and
    /// after each turn. Nothing polls.
    fn panel_branch(&mut self, envelope: &Envelope) -> Vec<String> {
        let mut send = Vec::new();
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = super::read!(envelope, CommandAccepted)
                    && self.panel_state.asked.as_deref() == Some(accepted.command_id.0.as_str())
                    && self.panel_state.answer_branch(accepted.result.as_ref())
                {
                    send.extend(self.branch_query());
                }
            }
            "command_rejected" => {
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
            "session_status" => {
                if let Some(status) = super::read!(envelope, SessionStatus) {
                    let busy = !matches!(status.state, SessionState::Idle | SessionState::Jobs);
                    if self.panel_state.turn_end(busy) {
                        if self.panel_state.asked.is_some() {
                            self.panel_state.again = true;
                        } else {
                            send.extend(self.branch_query());
                        }
                    }
                }
            }
            _ => {}
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
        if self.link != Link::Up {
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
        self.panel_state.asked = Some(id);
        Some(line)
    }

    /// A hub `command_rejected` for the branch query leaves the row out,
    /// with no notice, and clears the query so the next turn end sends.
    pub(super) fn panel_refused(&mut self, id: &str) -> Vec<String> {
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
                self.pending.insert(id, (Kind::Shell, draft));
                Effect::Send(vec![line])
            }
            Spot::Waiting => self.show_rail(),
            Spot::Tools => self.open_config_view(super::ConfigView::Tools),
            Spot::Widget => self.toggle_widget_row(),
        }
    }
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
