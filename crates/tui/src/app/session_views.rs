//! The attached session's swapped views (`docs/tui.md`, "Swapped views").
//!
//! These views fold the stream the terminal already reads and never fetch
//! state except when the view explicitly offers a command.

use super::{App, Effect};
use crate::context_view::{self, ContextFold, Sized};
use crate::keys::Key;
use crate::swapped::{Frame, List, Spot};
use crate::usage_view::UsageFold;
use contract::Envelope;
use contract::events::{DelegateStarted, JobStarted, UsageRecorded};

/// A session view the person can open from a command or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionView {
    /// `/usage`.
    Usage,
    /// `/context`.
    Context,
}

/// The selected row in the open usage view.
#[derive(Debug)]
enum Open {
    /// `/usage`.
    Usage(List),
    /// `/context`.
    Context(List),
}

/// The session views' state, reset for each attachment.
#[derive(Default)]
pub(in crate::app) struct SessionViews {
    open: Option<Open>,
    usage: UsageFold,
    rate: log::RateFold,
    context: ContextFold,
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
        self.session_views.open = Some(match view {
            SessionView::Usage => Open::Usage(List::default()),
            SessionView::Context => Open::Context(List::default()),
        });
        Effect::None
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
        match self.session_views.open.as_mut()? {
            Open::Usage(list) | Open::Context(list) => {
                list.key(key, rows, height);
            }
        }
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
        }
    }

    fn session_view_list_metrics(&self) -> Option<(usize, usize)> {
        let frame = self.session_view_screen(0)?;
        Some((
            frame.rows.len(),
            crate::swapped::rows_height(&frame, self.conversation_height()),
        ))
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
            _ => {}
        }
    }

    /// Closes the view and empties its fold for the next attachment.
    pub(in crate::app) fn session_views_reset(&mut self) {
        self.session_views = SessionViews::default();
    }

    /// Closes any open session view.
    pub(in crate::app) fn close_session_view(&mut self) {
        self.session_views.open = None;
    }
}

#[cfg(test)]
#[path = "session_views_tests.rs"]
mod tests;
