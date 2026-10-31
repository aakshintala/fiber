//! The attached session's swapped views (`docs/tui.md`, "Swapped views").
//!
//! These views fold the stream the terminal already reads and fetch a file's
//! diff only when the person chooses it.

use super::call_view::CallView;
use super::{App, Effect};
use crate::changed_files_view::{self, Diff};
use crate::context_view::{self, ContextFold, Sized};
use crate::keys::{Edit, Key};
use crate::shell;
use crate::swapped::{Frame, List, Spot};
use crate::usage_view::UsageFold;
use contract::Envelope;
use contract::events::{
    CommandAccepted, CommandRejected, DelegateStarted, JobStarted, ToolCallCompleted, UsageRecorded,
};

/// A session view the person can open from a command or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionView {
    /// `/usage`.
    Usage,
    /// `/quota`.
    Quota,
    /// `/context`.
    Context,
    /// `/status`.
    Status,
    /// The Changed files list.
    ChangedFiles,
    /// `/release-notes`.
    ReleaseNotes,
    /// The running delegates, from "N delegates running".
    Delegates,
    /// The running jobs, from "N jobs running".
    Jobs,
    /// `/delegates`.
    AllDelegates,
    /// A tool call's whole view, from its more row or its opened row.
    Call(usize),
    /// `/rewind`.
    Rewind,
    /// `/hooks`.
    Hooks,
}

/// The selected row in an open session view.
#[derive(Debug)]
pub(in crate::app) enum Open {
    /// `/usage`.
    Usage(List),
    /// `/quota`.
    Quota(List),
    /// `/context`.
    Context(List),
    /// `/status`.
    Status(List),
    /// Changed files' paths.
    Files(List),
    /// `/release-notes`.
    ReleaseNotes(List),
    /// The running delegates.
    Delegates(List),
    /// The running jobs.
    Jobs(List),
    /// `/delegates`.
    AllDelegates(List),
    /// A tool call's whole view, boxed: it holds the call's pages.
    Call(Box<CallView>),
    /// `/rewind`.
    Rewind(List),
    /// `/hooks`.
    Hooks(List),
}

/// The session views' state, reset for each attachment.
#[derive(Default)]
pub(in crate::app) struct SessionViews {
    open: Option<Open>,
    usage: UsageFold,
    rate: log::RateFold,
    context: ContextFold,
    request: Option<(String, String)>,
    diff: Option<(String, Diff)>,
    diff_list: List,
    figures_request: Option<String>,
    figures: Option<changed_files_view::NumstatAnswer>,
    hooks_request: Option<String>,
    hooks_rows: Option<Vec<contract::events::HookInfo>>,
}

impl App {
    /// The open call view, to read.
    pub(in crate::app) fn call_view(&self) -> Option<&CallView> {
        match self.session_views.open.as_ref() {
            Some(Open::Call(view)) => Some(view.as_ref()),
            _ => None,
        }
    }

    /// The open call view, to change.
    pub(in crate::app) fn call_view_mut(&mut self) -> Option<&mut CallView> {
        match self.session_views.open.as_mut() {
            Some(Open::Call(view)) => Some(view.as_mut()),
            _ => None,
        }
    }

    /// Opens the call's view built by the caller.
    pub(in crate::app) fn set_call_view(&mut self, view: CallView) {
        self.session_views.open = Some(Open::Call(Box::new(view)));
    }

    /// Opens `view` on the attached session. With no session the notice is
    /// pushed and nothing opens. The draft is left for panel and status-line
    /// clicks; a slash command clears it before calling this method.
    pub(crate) fn open_session_view(&mut self, view: SessionView) -> Effect {
        if matches!(view, SessionView::ReleaseNotes) {
            // The notes read the client's launch, never the session:
            // they open on home too, sending no driver command.
            self.model_picker.close();
            self.close_config_view();
            self.close_keymap();
            self.session_views.open = Some(Open::ReleaseNotes(List::default()));
            // Opening another view drops the rewind's wait: its late
            // answer folds nowhere.
            self.rewind = super::rewind::Rewind::default();
            return Effect::None;
        }
        if self.session().is_none() {
            self.notices
                .push("session_not_found", "No session on screen.".to_owned());
            return Effect::None;
        }
        self.model_picker.close();
        self.close_config_view();
        self.close_keymap();
        self.session_views.request = None;
        self.session_views.diff = None;
        self.session_views.diff_list = List::default();
        self.session_views.figures_request = None;
        self.session_views.open = Some(match view {
            SessionView::Usage => Open::Usage(List::default()),
            SessionView::Quota => Open::Quota(List::default()),
            SessionView::Context => Open::Context(List::default()),
            SessionView::Status => Open::Status(List::default()),
            SessionView::ChangedFiles => Open::Files(List::default()),
            SessionView::ReleaseNotes => Open::ReleaseNotes(List::default()),
            SessionView::Delegates => Open::Delegates(List::default()),
            SessionView::Jobs => Open::Jobs(List::default()),
            SessionView::AllDelegates => Open::AllDelegates(List::default()),
            SessionView::Call(id) => return self.open_call_view_inner(id),
            SessionView::Rewind => {
                self.session_views.open = Some(Open::Rewind(List::default()));
                return self.open_rewind();
            }
            SessionView::Hooks => {
                self.session_views.open = Some(Open::Hooks(List::default()));
                return self.open_hooks();
            }
        });
        // Opening another view drops the rewind's wait and the hooks query:
        // a late answer folds nowhere.
        self.rewind = super::rewind::Rewind::default();
        self.session_views.hooks_request = None;
        if matches!(view, SessionView::ChangedFiles) {
            return self.send_figures_request();
        }
        Effect::None
    }

    /// Opens Changed files and fetches `rank` from the displayed
    /// figures ordering. The open sends the numstat query first, so a
    /// file click yields its line then the diff line, in that order.
    pub(in crate::app) fn open_changed_file(&mut self, rank: usize) -> Effect {
        let effect = self.open_session_view(SessionView::ChangedFiles);
        if !self.session_view_open() {
            return effect;
        }
        match (effect, self.session_view_file_rank(rank)) {
            (Effect::Send(mut first), Effect::Send(mut second)) => {
                first.append(&mut second);
                Effect::Send(first)
            }
            (first, Effect::None) => first,
            (Effect::None, second) => second,
            (first, _) => first,
        }
    }

    /// Sends the Changed files figures query over the card's listed
    /// paths, like `open_hooks` sends its query: a late answer after
    /// another open folds nowhere, and with no driver the view opens
    /// showing the card's figures.
    fn send_figures_request(&mut self) -> Effect {
        self.session_views.figures_request = None;
        self.session_views.figures = None;
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        if !self.connected() {
            return Effect::None;
        }
        let paths: Vec<&str> = changed_files_view::ranked(self.panel_state.changes())
            .into_iter()
            .map(|(path, _, _)| path)
            .collect();
        if paths.is_empty() {
            return Effect::None;
        }
        let id = super::mint();
        let line = shell::command(
            &id,
            &session,
            &changed_files_view::numstat_command(&paths),
            false,
        )
        .to_string();
        self.session_views.figures_request = Some(id);
        Effect::Send(vec![line])
    }

    /// The Changed files rows' figures: the numstat answer resolved
    /// over the card's sums, or the card's sums until it answers. The
    /// list and the Enter rank share this, so rows and fetching
    /// always agree.
    fn displayed_figures(&self) -> changed_files_view::ResolvedFigures {
        match self.session_views.figures.as_ref() {
            Some(answer) => changed_files_view::resolve_figures(
                self.panel_state.changes(),
                Some(answer.raw.as_str()),
                &answer.lines,
                self.workspace_root(),
            ),
            None => changed_files_view::resolve_figures(
                self.panel_state.changes(),
                None,
                &std::collections::BTreeMap::new(),
                self.workspace_root(),
            ),
        }
    }

    /// Whether a session view is open.
    pub(crate) fn session_view_open(&self) -> bool {
        self.session_views.open.is_some()
    }

    /// Hands `key` to the open session view. `None` with no view open or
    /// for Ctrl+C.
    pub(in crate::app) fn session_view_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.session_view_open() || *key == Key::CtrlC {
            return None;
        }
        if matches!(self.session_views.open, Some(Open::Call(_))) {
            return self.call_view_key(key);
        }
        if matches!(self.session_views.open, Some(Open::Rewind(_))) {
            return self.rewind_key(key);
        }
        if *key == Key::Esc {
            self.close_session_view();
            return Some(Effect::None);
        }
        let (rows, height) = self.session_view_list_metrics()?;
        if matches!(
            self.session_views.open,
            Some(Open::Delegates(_)) | Some(Open::Jobs(_)) | Some(Open::AllDelegates(_))
        ) {
            // Enter resolves the selected row to its serial at key time,
            // where no press/release race exists; opening the item's view
            // closes the list.
            if *key == Key::Enter {
                return Some(self.session_view_open_selected());
            }
            if let Some(Open::Delegates(list) | Open::Jobs(list) | Open::AllDelegates(list)) =
                self.session_views.open.as_mut()
            {
                list.key(key, rows, height);
            }
            return Some(Effect::None);
        }
        if let Some(Open::Files(list)) = self.session_views.open.as_ref() {
            if self.session_views.diff.is_some() {
                self.session_views.diff_list.key(key, rows, height);
            } else if *key == Key::Enter {
                return Some(self.session_view_file_rank(list.selected()));
            } else if let Some(Open::Files(list)) = self.session_views.open.as_mut() {
                list.key(key, rows, height);
            }
            return Some(Effect::None);
        }
        if let Some(
            Open::Usage(list)
            | Open::Quota(list)
            | Open::Context(list)
            | Open::Status(list)
            | Open::ReleaseNotes(list)
            | Open::Hooks(list),
        ) = self.session_views.open.as_mut()
        {
            list.key(key, rows, height);
        }
        Some(Effect::None)
    }

    /// Shifts the open session view's window one wheel step through its
    /// own entry point: the call's viewport and the rewind list through
    /// theirs, every other list three rows with its selection kept while
    /// shown. `None` with no view open. Never an Up/Down step: the keys
    /// keep their own path.
    pub(in crate::app) fn session_view_wheel(&mut self, up: bool) -> Option<Effect> {
        if matches!(self.session_views.open, Some(Open::Call(_))) {
            return Some(self.call_view_wheel(up));
        }
        if matches!(self.session_views.open, Some(Open::Rewind(_))) {
            return Some(self.rewind_wheel(up));
        }
        let (rows, height) = self.session_view_list_metrics()?;
        if self.session_views.diff.is_some() {
            self.session_views.diff_list.wheel(up, rows, height, 3);
            return Some(Effect::None);
        }
        if let Some(
            Open::Delegates(list)
            | Open::Jobs(list)
            | Open::AllDelegates(list)
            | Open::Files(list)
            | Open::Usage(list)
            | Open::Quota(list)
            | Open::Context(list)
            | Open::Status(list)
            | Open::ReleaseNotes(list)
            | Open::Hooks(list),
        ) = self.session_views.open.as_mut()
        {
            list.wheel(up, rows, height, 3);
        }
        Some(Effect::None)
    }

    /// Hands the horizontal-left edit key to the diff view. Other editing
    /// keys are swallowed while a session view is open.
    pub(in crate::app) fn session_view_edit(&mut self, edit: &Edit) -> Option<Effect> {
        if !self.session_view_open() {
            return None;
        }
        if *edit != Edit::Left {
            return Some(Effect::None);
        }
        if !matches!(self.session_views.open, Some(Open::Files(_))) {
            return Some(Effect::None);
        }
        if self.session_views.diff.is_none() {
            return Some(Effect::None);
        }
        self.session_views.request = None;
        self.session_views.diff = None;
        self.session_views.diff_list = List::default();
        Some(Effect::None)
    }

    /// Handles a click on the open view's close target or a row.
    pub(in crate::app) fn session_view_click(&mut self, spot: Spot) -> Effect {
        if matches!(
            self.session_views.open,
            Some(Open::Delegates(_)) | Some(Open::Jobs(_)) | Some(Open::AllDelegates(_))
        ) {
            // A click on the text opens the row's serial, running or
            // finished; a click on the blank tail only selects the row.
            match spot {
                Spot::Close => self.close_session_view(),
                Spot::Item(serial) => return self.open_serial(serial),
                Spot::Row(at) => {
                    if let Some((rows, height)) = self.session_view_list_metrics()
                        && let Some(
                            Open::Delegates(list) | Open::Jobs(list) | Open::AllDelegates(list),
                        ) = self.session_views.open.as_mut()
                    {
                        list.select(at, rows, height);
                    }
                }
                Spot::Cell(_, _) | Spot::Switch { .. } | Spot::Revoke(_) => {}
            }
            return Effect::None;
        }
        if matches!(self.session_views.open, Some(Open::Call(_))) {
            // The top row closes the view with the call's row still
            // focused, as Esc does; a body row selects it.
            if spot == Spot::Close {
                self.close_call_view_inner();
                return Effect::None;
            }
            return self.call_view_click(&spot);
        }
        if let Some(Open::Rewind(_)) = self.session_views.open {
            return self.rewind_click(spot);
        }
        if spot == Spot::Close {
            self.close_session_view();
            return Effect::None;
        }
        let Some((rows, height)) = self.session_view_list_metrics() else {
            return Effect::None;
        };
        if matches!(self.session_views.open, Some(Open::Files(_))) {
            if self.session_views.diff.is_some() {
                if let Spot::Row(at) = spot {
                    self.session_views.diff_list.select(at, rows, height);
                }
                return Effect::None;
            }
            if let Spot::Row(at) = spot {
                if let Some(Open::Files(list)) = self.session_views.open.as_mut() {
                    list.select(at, rows, height);
                }
                return self.session_view_file_rank(at);
            }
            return Effect::None;
        }
        if let Spot::Row(at) = spot
            && let Some(
                Open::Usage(list)
                | Open::Quota(list)
                | Open::Context(list)
                | Open::Status(list)
                | Open::ReleaseNotes(list)
                | Open::Hooks(list),
            ) = self.session_views.open.as_mut()
        {
            list.select(at, rows, height);
        }
        Effect::None
    }

    /// Opens the selected list row's item: the row resolves to its serial
    /// at key time; a serial whose job is gone opens nothing, and a
    /// finished job opens its replay.
    fn session_view_open_selected(&mut self) -> Effect {
        let selected = match self.session_views.open.as_ref() {
            Some(Open::Delegates(list))
            | Some(Open::Jobs(list))
            | Some(Open::AllDelegates(list)) => list.selected(),
            Some(Open::Usage(_))
            | Some(Open::Quota(_))
            | Some(Open::Context(_))
            | Some(Open::Status(_))
            | Some(Open::Files(_))
            | Some(Open::ReleaseNotes(_))
            | Some(Open::Call(_))
            | Some(Open::Rewind(_))
            | Some(Open::Hooks(_))
            | None => {
                return Effect::None;
            }
        };
        let serial = self
            .session_view_list_serials()
            .get(selected)
            .copied()
            .flatten();
        match serial {
            Some(serial) => self.open_serial(serial),
            None => Effect::None,
        }
    }

    /// The open list's rows in frame order: each row's job serial, `None`
    /// where the item fold names none. A delegate fills two rows with one
    /// serial; a job fills one.
    fn session_view_list_serials(&self) -> Vec<Option<u64>> {
        match self.session_views.open.as_ref() {
            Some(Open::Delegates(_) | Open::Jobs(_) | Open::AllDelegates(_)) => self
                .session_view_screen(0)
                .into_iter()
                .flat_map(|frame| frame.rows)
                .map(|cells| {
                    cells.into_iter().find_map(|(_, spot, _)| match spot {
                        Some(Spot::Item(serial)) => Some(serial),
                        _ => None,
                    })
                })
                .collect(),
            Some(Open::Usage(_))
            | Some(Open::Quota(_))
            | Some(Open::Context(_))
            | Some(Open::Status(_))
            | Some(Open::Files(_))
            | Some(Open::ReleaseNotes(_))
            | Some(Open::Call(_))
            | Some(Open::Rewind(_))
            | Some(Open::Hooks(_))
            | None => Vec::new(),
        }
    }

    /// The open session view's frame. Every view's row count except the
    /// release notes' is independent of `width`, which lets key handling
    /// count rows at width zero; the notes wrap at the width, so width
    /// zero resolves to the width the draw uses.
    pub(crate) fn session_view_screen(&self, width: u16) -> Option<Frame> {
        match self.session_views.open.as_ref()? {
            Open::Usage(list) => Some(crate::usage_view::frame(
                &self.session_views.usage,
                self.panel_state.budget(),
                *list,
            )),
            Open::Quota(list) => Some(crate::quota_view::frame(
                &self.panel_state.quota.rows,
                self.panel_state.quota.notice_at,
                self.panel_state.quota.fetching(),
                *list,
                width,
                epoch_ms(self),
            )),
            Open::Context(list) => {
                let sized = self.panel_state.status().and_then(|status| {
                    let context = status.context.as_ref()?;
                    Some(Sized {
                        total: context.tokens,
                        window: self.panel_state.window()?,
                        trigger: self.panel_state.trigger_at(),
                    })
                });
                Some(context_view::frame(
                    &self.session_views.context,
                    self.session_views.rate.rate(),
                    sized,
                    *list,
                    width,
                ))
            }
            Open::Status(list) => Some(crate::status_view::frame(
                &crate::status_view::Facts {
                    session: self.session(),
                    status: self.panel_state.status(),
                    credential: self.panel_state.credential(),
                    hub: self
                        .home
                        .as_ref()
                        .and_then(|home| match &home.launch.location {
                            crate::LaunchLocation::Local { .. } => None,
                            crate::LaunchLocation::Remote { hub } => Some(hub.as_str()),
                        }),
                    version: self.home.as_ref().map(|home| home.launch.version.as_str()),
                },
                *list,
            )),
            Open::Files(list) => {
                let chosen = self
                    .session_views
                    .diff
                    .as_ref()
                    .map(|(path, diff)| (path.as_str(), diff));
                let list = if chosen.is_some() {
                    self.session_views.diff_list
                } else {
                    *list
                };
                Some(changed_files_view::frame(
                    self.panel_state.changes(),
                    Some(&self.displayed_figures()),
                    chosen,
                    list,
                ))
            }
            Open::ReleaseNotes(list) => {
                let home = self.home.as_ref()?;
                // Width zero means a metrics call, not a draw: wrap at
                // the width the draw uses.
                let width = if width == 0 {
                    self.notes_width()
                } else {
                    width
                };
                Some(crate::release_notes::frame(
                    home.launch.release_notes,
                    home.launch.version.as_str(),
                    *list,
                    width,
                    self.notes_height(),
                ))
            }
            Open::Delegates(list) => Some(crate::running_view::delegates_frame(self, *list)),
            Open::Jobs(list) => Some(crate::running_view::jobs_frame(self, *list)),
            Open::AllDelegates(list) => Some(crate::running_view::all_delegates_frame(self, *list)),
            Open::Call(view) => Some(view.frame()),
            Open::Rewind(list) => {
                let jobs = self.rewind_stopping();
                Some(super::rewind::frame(&self.rewind, &jobs, *list))
            }
            Open::Hooks(list) => {
                let rows = self.session_views.hooks_rows.as_deref().unwrap_or(&[]);
                Some(super::hooks_view::frame(rows, *list, width))
            }
        }
    }

    fn session_view_list_metrics(&self) -> Option<(usize, usize)> {
        let frame = self.session_view_screen(0)?;
        // The notes page the drawn height: the conversation's attached,
        // the whole screen's on home.
        let height = if matches!(self.session_views.open, Some(Open::ReleaseNotes(_))) {
            self.notes_height()
        } else {
            self.conversation_height()
        };
        Some((
            frame.rows.len(),
            crate::swapped::rows_height(&frame, height),
        ))
    }

    /// The width the release-notes frame wraps at when no draw width is
    /// given: the conversation column attached, the whole screen on
    /// home, matching the draw.
    fn notes_width(&self) -> u16 {
        if self.session().is_some() {
            self.column_width()
        } else {
            self.screen_size().0
        }
    }

    /// The height the release-notes frame pages at: the conversation's
    /// attached, the whole screen's on home, matching the draw.
    fn notes_height(&self) -> usize {
        if self.session().is_some() {
            self.conversation_height()
        } else {
            usize::from(self.screen_size().1)
        }
    }

    /// Starts the selected file's diff request, or leaves an empty list when
    /// `rank` no longer names a file in the displayed figures ordering.
    fn session_view_file_rank(&mut self, rank: usize) -> Effect {
        let displayed = self.displayed_figures();
        let Some((path, _, _)) = changed_files_view::ranked(&displayed.figures)
            .get(rank)
            .copied()
        else {
            return Effect::None;
        };
        let path = path.to_owned();
        if !self.connected() {
            self.session_views.request = None;
            self.session_views.diff = Some((path, Diff::Failed("Not connected.".to_owned())));
            return Effect::None;
        }
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        let id = super::mint();
        let line = shell::command(
            &id,
            &session,
            &changed_files_view::diff_command(&path),
            false,
        )
        .to_string();
        self.session_views.request = Some((id, path.clone()));
        self.session_views.diff = Some((path, Diff::Reading));
        self.session_views.diff_list = List::default();
        Effect::Send(vec![line])
    }

    /// Folds one line from the attached session into the session views,
    /// returning the call view's follow-up fetch lines.
    pub(in crate::app) fn session_views_line(&mut self, envelope: &Envelope) -> Vec<String> {
        self.session_views.rate.fold(envelope);
        self.session_views.context.fold(envelope);
        match envelope.kind.as_str() {
            "turn_started" => {
                if let Some(turn) = &envelope.turn_id {
                    self.session_views.usage.turn_started(turn.clone());
                }
            }
            "job_started" => {
                if let Some(started) = super::read!(envelope, JobStarted) {
                    self.session_views
                        .usage
                        .job_started(started.job_id, started.description);
                }
            }
            "delegate_started" => {
                if let Some(started) = super::read!(envelope, DelegateStarted) {
                    self.session_views.usage.delegate_started(&started);
                }
            }
            "usage_recorded" => {
                if let Some(recorded) = super::read!(envelope, UsageRecorded) {
                    self.session_views
                        .usage
                        .recorded(envelope.turn_id.clone(), recorded);
                }
            }
            "command_accepted" => {
                if let Some(accepted) = super::read!(envelope, CommandAccepted) {
                    // A pending fork attaches before any open view
                    // handles the answer: opening another view resets
                    // the rewind view, not the fork.
                    if self.fork_pending.as_ref() == Some(&accepted.command_id)
                        && let Some(lines) = self.rewind_send_accepted(&accepted)
                    {
                        return lines;
                    }
                    if matches!(self.session_views.open, Some(Open::Call(_))) {
                        return self.call_view_answered(&accepted);
                    }
                    if matches!(self.session_views.open, Some(Open::Rewind(_))) {
                        if let Some(lines) = self.rewind_send_accepted(&accepted) {
                            return lines;
                        }
                        return self.rewind_answered(envelope, &accepted);
                    }
                    if matches!(self.session_views.open, Some(Open::Hooks(_))) {
                        self.hooks_answered(&accepted);
                        return Vec::new();
                    }
                    self.session_views_answered(&accepted);
                }
            }
            "command_rejected" => {
                if let Some(rejected) = super::read!(envelope, CommandRejected) {
                    // A pending fork's refusal shows as a notice before
                    // any open view folds it: opening another view resets
                    // the rewind view, not the fork.
                    if self.rewind_fork_refused(&rejected) {
                        return Vec::new();
                    }
                    if matches!(self.session_views.open, Some(Open::Call(_))) {
                        self.call_view_refused(&rejected);
                    } else if matches!(self.session_views.open, Some(Open::Rewind(_))) {
                        self.rewind_refused(&rejected);
                    } else if matches!(self.session_views.open, Some(Open::Hooks(_))) {
                        self.hooks_refused(&rejected);
                    } else {
                        self.session_views_rejected(&rejected);
                    }
                }
            }
            "tool_call_completed" => {
                if let Some(done) = super::read!(envelope, ToolCallCompleted)
                    && matches!(self.session_views.open, Some(Open::Call(_)))
                    && let Some(action) = envelope.action_id.as_ref()
                {
                    return self.refresh_call_view(&envelope.session_id, &action.0, &done);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// A session answer fills only the diff request that still owns its id.
    fn session_views_answered(&mut self, accepted: &CommandAccepted) {
        if self.session_views.figures_request.as_deref() == Some(accepted.command_id.0.as_str()) {
            self.session_views.figures_request = None;
            self.session_views.figures = self.answered_figures(accepted);
        }
        let Some((id, path)) = self.session_views.request.as_ref() else {
            return;
        };
        if id != &accepted.command_id.0 {
            return;
        }
        let path = path.clone();
        self.session_views.request = None;
        self.session_views.diff =
            Some((path, changed_files_view::answer(accepted.result.as_ref())));
    }

    /// Folds a numstat answer into stored figures: git's raw output
    /// plus the workspace line counts for the paths it omits.
    /// Anything but a readable numstat answer stores nothing, so
    /// the rows keep the card's figures and the view opens normally.
    fn answered_figures(
        &self,
        accepted: &CommandAccepted,
    ) -> Option<changed_files_view::NumstatAnswer> {
        let Some(contract::events::CommandResult::Shell {
            output, process, ..
        }) = accepted.result.as_ref()
        else {
            return None;
        };
        if process.signal.is_some() || process.timed_out {
            return None;
        }
        // Only a clean run counts: any other exit leaves the rows on the
        // card's figures with the view opening normally.
        if process.exit_code != Some(0) {
            return None;
        }
        let body = changed_files_view::numstat_body(output, process.exit_code);
        if !changed_files_view::numstat_fully_parsed(body) {
            return None;
        }
        let raw = body.to_owned();
        let parsed = changed_files_view::parse_numstat(&raw);
        let omitted: Vec<&str> = self
            .panel_state
            .changes()
            .keys()
            .filter(|path| {
                changed_files_view::lookup(&parsed, path, self.workspace_root()).is_none()
            })
            .map(String::as_str)
            .collect();
        let lines = match self.workspace_root() {
            Some(root) => changed_files_view::count_lines(root, &omitted),
            None => std::collections::BTreeMap::new(),
        };
        Some(changed_files_view::NumstatAnswer { raw, lines })
    }

    /// The local launch workspace, when the terminal has one: the
    /// root the Changed files paths read against.
    fn workspace_root(&self) -> Option<&std::path::Path> {
        match self.home.as_ref().map(|home| &home.launch.location) {
            Some(crate::LaunchLocation::Local { workspace, .. }) => Some(workspace),
            _ => None,
        }
    }

    /// A session rejection fills only the diff request that still owns its id.
    fn session_views_rejected(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        self.session_views_refused(&id.0, &rejected.message);
    }

    /// A hub `command_rejected` for the current diff request shows its message.
    pub(in crate::app) fn session_views_refused(&mut self, id: &str, message: &str) {
        if self.session_views.figures_request.as_deref() == Some(id) {
            self.session_views.figures_request = None;
        }
        let Some((request, path)) = self.session_views.request.as_ref() else {
            return;
        };
        if request != id {
            return;
        }
        let path = path.clone();
        self.session_views.request = None;
        self.session_views.diff = Some((path, Diff::Failed(message.to_owned())));
    }

    /// Closes the view and empties its fold for the next attachment.
    pub(in crate::app) fn session_views_reset(&mut self) {
        self.session_views = SessionViews::default();
        // Leaving the session drops the rewind view's fold with it: the
        // next open pages the new session's chain from its tip.
        self.rewind = super::rewind::Rewind::default();
    }

    /// Opens the hooks view on the attached session and sends its `hooks`
    /// query. A late answer after another view replaces it folds nowhere,
    /// as does an answer to an older query after a newer one.
    pub(in crate::app) fn open_hooks(&mut self) -> Effect {
        self.session_views.hooks_request = None;
        self.session_views.hooks_rows = None;
        let Some(session) = self.session().cloned() else {
            return Effect::None;
        };
        if !self.connected() {
            return Effect::None;
        }
        let id = super::mint();
        let line = super::session_command(&id, "hooks", &session, None).to_string();
        self.session_views.hooks_request = Some(id);
        Effect::Send(vec![line])
    }

    /// Folds a `command_accepted` into the open hooks view when it answers
    /// the pending `hooks` query: a late answer, another view's answer, or
    /// another command's shape folds nowhere.
    pub(in crate::app) fn hooks_answered(&mut self, accepted: &CommandAccepted) {
        let Some(request) = self.session_views.hooks_request.as_ref() else {
            return;
        };
        if request != &accepted.command_id.0 {
            return;
        }
        let Some(contract::events::CommandResult::Hooks { hooks }) = accepted.result.as_ref()
        else {
            return;
        };
        self.session_views.hooks_request = None;
        self.session_views.hooks_rows = Some(hooks.clone());
    }

    /// A `command_rejected` for the pending `hooks` query drops the wait:
    /// the view keeps showing what it holds, if anything.
    pub(in crate::app) fn hooks_refused(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        if self.session_views.hooks_request.as_deref() != Some(&id.0) {
            return;
        }
        self.session_views.hooks_request = None;
    }

    /// Moves the open rewind view's list to `at`: the fold's selection.
    /// A closed or missing view moves nothing.
    pub(in crate::app) fn rewind_list_to(&mut self, at: usize, rows: usize) {
        if let Some(Open::Rewind(list)) = self.session_views.open.as_mut() {
            list.select(at, rows, usize::MAX);
        }
    }

    /// The open rewind view's list, to move with the cursor.
    pub(in crate::app) fn rewind_list_mut(&mut self) -> Option<&mut List> {
        match self.session_views.open.as_mut() {
            Some(Open::Rewind(list)) => Some(list),
            _ => None,
        }
    }

    /// Closes the view and drops any answer that arrives for its diff request.
    pub(in crate::app) fn close_session_view(&mut self) {
        self.session_views.open = None;
        self.session_views.request = None;
        self.session_views.diff = None;
        self.session_views.diff_list = List::default();
        self.session_views.hooks_request = None;
        self.session_views.hooks_rows = None;
        self.rewind = super::rewind::Rewind::default();
    }
}

/// The wall clock in milliseconds since the epoch, for the quota view's
/// reset times: the frame's injected wall time, zero before it is set.
fn epoch_ms(app: &App) -> u64 {
    app.motion().wall_ms().unwrap_or(0)
}

#[cfg(test)]
#[path = "session_views_tests.rs"]
mod tests;
