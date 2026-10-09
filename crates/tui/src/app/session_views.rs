//! The attached session's swapped views (`docs/tui.md`, "Swapped views").
//!
//! These views fold the stream the terminal already reads and fetch a file's
//! diff only when the person chooses it.

use super::{App, Effect};
use crate::changed_files_view::{self, Diff};
use crate::context_view::{self, ContextFold, Sized};
use crate::keys::{Edit, Key};
use crate::shell;
use crate::swapped::{Frame, List, Spot};
use crate::usage_view::UsageFold;
use contract::Envelope;
use contract::events::{
    CommandAccepted, CommandRejected, DelegateStarted, JobStarted, UsageRecorded,
};

/// A session view the person can open from a command or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionView {
    /// `/usage`.
    Usage,
    /// `/context`.
    Context,
    /// The Changed files list.
    ChangedFiles,
}

/// The selected row in an open session view.
#[derive(Debug)]
enum Open {
    /// `/usage`.
    Usage(List),
    /// `/context`.
    Context(List),
    /// Changed files' paths.
    Files(List),
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
}

impl App {
    /// Opens `view` on the attached session. With no session the notice is
    /// pushed and nothing opens. The draft is left for panel and status-line
    /// clicks; a slash command clears it before calling this method.
    pub(crate) fn open_session_view(&mut self, view: SessionView) -> Effect {
        if self.session().is_none() {
            self.notices.push("No session on screen.".to_owned());
            return Effect::None;
        }
        self.model_picker.close();
        self.close_config_view();
        self.close_keymap();
        self.session_views.request = None;
        self.session_views.diff = None;
        self.session_views.diff_list = List::default();
        self.session_views.open = Some(match view {
            SessionView::Usage => Open::Usage(List::default()),
            SessionView::Context => Open::Context(List::default()),
            SessionView::ChangedFiles => Open::Files(List::default()),
        });
        Effect::None
    }

    /// Opens Changed files and fetches `rank` from the current card ordering.
    pub(in crate::app) fn open_changed_file(&mut self, rank: usize) -> Effect {
        let effect = self.open_session_view(SessionView::ChangedFiles);
        if !self.session_view_open() {
            return effect;
        }
        self.session_view_file_rank(rank)
    }

    /// Whether a session view is open.
    pub(crate) fn session_view_open(&self) -> bool {
        self.session_views.open.is_some()
    }

    /// Hands `key` to the open session view. `None` with no view open, for
    /// Ctrl+C, or while the quit question is open.
    pub(in crate::app) fn session_view_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.session_view_open() || self.quit_open() || *key == Key::CtrlC {
            return None;
        }
        if *key == Key::Esc {
            self.close_session_view();
            return Some(Effect::None);
        }
        let (rows, height) = self.session_view_list_metrics()?;
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
        if let Some(Open::Usage(list) | Open::Context(list)) = self.session_views.open.as_mut() {
            list.key(key, rows, height);
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
            && let Some(Open::Usage(list) | Open::Context(list)) = self.session_views.open.as_mut()
        {
            list.select(at, rows, height);
        }
        Effect::None
    }

    /// The open session view's frame. Its row count does not depend on
    /// `width`, which lets key handling count rows at width zero.
    pub(crate) fn session_view_screen(&self, width: u16) -> Option<Frame> {
        match self.session_views.open.as_ref()? {
            Open::Usage(list) => Some(crate::usage_view::frame(
                &self.session_views.usage,
                self.panel_state.budget(),
                *list,
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
                    chosen,
                    list,
                ))
            }
        }
    }

    fn session_view_list_metrics(&self) -> Option<(usize, usize)> {
        let frame = self.session_view_screen(0)?;
        Some((
            frame.rows.len(),
            crate::swapped::rows_height(&frame, self.conversation_height()),
        ))
    }

    /// Starts the selected file's diff request, or leaves an empty list when
    /// `rank` no longer names a file in the card ordering.
    fn session_view_file_rank(&mut self, rank: usize) -> Effect {
        let Some((path, _, _)) = changed_files_view::ranked(self.panel_state.changes())
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

    /// Folds one line from the attached session into the session views.
    pub(in crate::app) fn session_views_line(&mut self, envelope: &Envelope) {
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
                    self.session_views_answered(&accepted);
                }
            }
            "command_rejected" => {
                if let Some(rejected) = super::read!(envelope, CommandRejected) {
                    self.session_views_rejected(&rejected);
                }
            }
            _ => {}
        }
    }

    /// A session answer fills only the diff request that still owns its id.
    fn session_views_answered(&mut self, accepted: &CommandAccepted) {
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

    /// A session rejection fills only the diff request that still owns its id.
    fn session_views_rejected(&mut self, rejected: &CommandRejected) {
        let Some(id) = rejected.command_id.as_ref() else {
            return;
        };
        self.session_views_refused(&id.0, &rejected.message);
    }

    /// A hub `command_rejected` for the current diff request shows its message.
    pub(in crate::app) fn session_views_refused(&mut self, id: &str, message: &str) {
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
    }

    /// Closes the view and drops any answer that arrives for its diff request.
    pub(in crate::app) fn close_session_view(&mut self) {
        self.session_views.open = None;
        self.session_views.request = None;
        self.session_views.diff = None;
        self.session_views.diff_list = List::default();
    }
}

#[cfg(test)]
#[path = "session_views_tests.rs"]
mod tests;
