//! The configuration views on the app (`docs/tui.md`, "Swapped views"):
//! which one is open, the seam they read and write through, the theme a
//! choice queued for the loop, and the last call's prompt size, which a
//! reload's cache rebuild costs. One view is open at a time; while it is,
//! it takes every key but Ctrl+C.

use std::sync::Arc;

use contract::events::UsageRecorded;
use contract::{Envelope, SessionId};

use super::{App, Effect};
use crate::ThemeSetting;
use crate::configure::Configure;
use crate::keys::{Edit, Key};
use crate::rules_view::Rules;
use crate::settings_view::{Act, Ctx, Settings};
use crate::swapped::{Frame, List, Spot};

/// What a view says when the terminal was given no seam.
const UNAVAILABLE: &str = "Not available in this terminal.";

/// A configuration view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigView {
    /// `/settings`.
    Settings,
    /// `/rules`.
    Rules,
}

/// The open view.
#[derive(Debug)]
enum Open {
    /// `/settings`.
    Settings(Settings),
    /// `/rules`.
    Rules(Rules),
    /// A view with no seam to read through, by its title.
    Unavailable(&'static str),
}

/// The configuration views' state.
#[derive(Default)]
pub(in crate::app) struct ConfigViews {
    /// The seam `main` passed, if any.
    seam: Option<Arc<dyn Configure>>,
    /// The open view.
    open: Option<Open>,
    /// A theme chosen and not yet applied by the loop.
    theme: Option<ThemeSetting>,
    /// The latest call's prompt size in tokens, and its session.
    usage: Option<(SessionId, u64)>,
}

impl App {
    /// Sets the seam the views read and write through.
    pub(crate) fn set_configure(&mut self, seam: Option<Arc<dyn Configure>>) {
        self.config_views.seam = seam;
    }

    /// Opens `view`, closing any open, its unsaved edit dropped. With no
    /// seam the view says it is not available.
    pub(crate) fn open_config_view(&mut self, view: ConfigView) -> Effect {
        self.draft.clear();
        self.config_views.open = None;
        let open = match (self.config_views.seam.clone(), view) {
            (Some(seam), ConfigView::Settings) => {
                let workspace = self.workspace();
                Open::Settings(Settings::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (Some(seam), ConfigView::Rules) => {
                let workspace = self.workspace();
                Open::Rules(Rules::open(&self.config_ctx(seam.as_ref(), &workspace)))
            }
            (None, ConfigView::Settings) => Open::Unavailable("Settings"),
            (None, ConfigView::Rules) => Open::Unavailable("Rules"),
        };
        self.config_views.open = Some(open);
        Effect::None
    }

    /// Whether a configuration view is open.
    pub(crate) fn config_view_open(&self) -> bool {
        self.config_views.open.is_some()
    }

    /// What a call needs: the seam, the workspace, the view's rows and the
    /// last call's prompt size on the session on screen.
    fn config_ctx<'a>(&self, seam: &'a dyn Configure, workspace: &'a std::path::Path) -> Ctx<'a> {
        let height = if self.on_home() {
            usize::from(self.screen.height())
        } else {
            self.conversation_height()
        };
        let usage = self
            .config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens);
        Ctx {
            seam,
            workspace,
            height,
            usage,
        }
    }

    /// Hands `key` to the open view. `None` with no view open, and for
    /// Ctrl+C and while the quit question is open, which go on to the
    /// quit flow.
    pub(in crate::app) fn config_view_key(&mut self, key: &Key) -> Option<Effect> {
        if self.config_views.open.is_none() || self.quit_open() || *key == Key::CtrlC {
            return None;
        }
        let act = match self.config_views.seam.clone() {
            Some(seam) => {
                let workspace = self.workspace();
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => settings.key(key, &ctx),
                    Some(Open::Rules(rules)) => rules.key(key, &ctx),
                    Some(Open::Unavailable(_)) | None => esc(key),
                }
            }
            None => esc(key),
        };
        Some(self.config_act(act))
    }

    /// Hands an editing key to the open view's field. `None` with no view
    /// open.
    pub(in crate::app) fn config_view_edit(&mut self, edit: &Edit) -> Option<Effect> {
        self.config_views.open.as_ref()?;
        let act = match self.config_views.seam.clone() {
            Some(seam) => {
                let workspace = self.workspace();
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => {
                        settings.edit_key(edit.clone());
                        Act::Stay
                    }
                    Some(Open::Rules(rules)) => rules.edit_key(edit, &ctx),
                    Some(Open::Unavailable(_)) | None => Act::Stay,
                }
            }
            None => Act::Stay,
        };
        Some(self.config_act(act))
    }

    /// A click on the open view's `spot`.
    pub(in crate::app) fn config_view_click(&mut self, spot: Spot) -> Effect {
        let act = match (self.config_views.seam.clone(), spot) {
            (_, Spot::Close) => Act::Close,
            (Some(seam), Spot::Row(_) | Spot::Revoke(_)) => {
                let workspace = self.workspace();
                let ctx = self.config_ctx(seam.as_ref(), &workspace);
                match &mut self.config_views.open {
                    Some(Open::Settings(settings)) => settings.click(spot, &ctx),
                    Some(Open::Rules(rules)) => rules.click(spot, &ctx),
                    Some(Open::Unavailable(_)) | None => Act::Stay,
                }
            }
            (None, Spot::Row(_) | Spot::Revoke(_)) => Act::Stay,
            // Only the model picker draws cells with targets of their
            // own; here a cell is never pushed.
            (_, Spot::Cell(_, _)) => Act::Stay,
        };
        self.config_act(act)
    }

    /// Carries out what the view asked.
    fn config_act(&mut self, act: Act) -> Effect {
        match act {
            Act::Stay => Effect::None,
            Act::Close => {
                self.config_views.open = None;
                Effect::None
            }
            Act::Theme(setting) => {
                self.config_views.theme = Some(setting);
                Effect::None
            }
            Act::Open(file) => Effect::OpenFile(file),
        }
    }

    /// Closes the open view, if one is open: one swapped view shows at
    /// a time, so opening the model picker closes a configuration view.
    pub(in crate::app) fn close_config_view(&mut self) {
        self.config_views.open = None;
    }

    /// The open view's frame.
    pub(crate) fn config_view_screen(&self) -> Option<Frame> {
        let usage = self
            .config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens);
        Some(match self.config_views.open.as_ref()? {
            Open::Settings(settings) => settings.frame(usage),
            Open::Rules(rules) => rules.frame(),
            Open::Unavailable(title) => Frame {
                title: (*title).to_owned(),
                rows: Vec::new(),
                list: List::default(),
                below: vec![UNAVAILABLE.to_owned()],
                field: None,
                footer: "Esc close".to_owned(),
            },
        })
    }

    /// The last call's prompt size on the session on screen, if its
    /// call was the session's own: what a reload's cache rebuild costs.
    /// A copied or extension call, or another session's, counts nothing.
    pub(in crate::app) fn usage_on_screen(&self) -> Option<u64> {
        self.config_views
            .usage
            .as_ref()
            .filter(|(session, _)| self.session() == Some(session))
            .map(|(_, tokens)| *tokens)
    }

    /// The theme a choice queued, once.
    pub(crate) fn take_theme_choice(&mut self) -> Option<ThemeSetting> {
        self.config_views.theme.take()
    }

    /// The editor Ctrl+G opened returned: the open view reads its rows
    /// again, and an error is a notice.
    pub(crate) fn config_file_closed(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.notices.push(message);
        }
        if let Some(seam) = self.config_views.seam.clone() {
            let workspace = self.workspace();
            let ctx = self.config_ctx(seam.as_ref(), &workspace);
            match &mut self.config_views.open {
                Some(Open::Settings(settings)) => settings.reread(&ctx),
                Some(Open::Rules(rules)) => rules.reread(&ctx),
                Some(Open::Unavailable(_)) | None => {}
            }
        }
    }

    /// A `usage_recorded` on the session on screen: a call of its own,
    /// not a copy or an extension's, sets the prompt size a reload's cache
    /// rebuild costs: its input, cache read and cache write tokens.
    pub(in crate::app) fn config_views_usage(&mut self, envelope: &Envelope) {
        let Some(usage) = super::read!(envelope, UsageRecorded) else {
            return;
        };
        if usage.origin_session_id.is_some() || usage.extension.is_some() {
            return;
        }
        let tokens = usage.tokens.cache_write.values().fold(
            usage.tokens.input.saturating_add(usage.tokens.cache_read),
            |sum, n| sum.saturating_add(*n),
        );
        self.config_views.usage = Some((envelope.session_id.clone(), tokens));
    }
}

/// A view with nothing to show closes on Esc and does nothing else.
fn esc(key: &Key) -> Act {
    if *key == Key::Esc {
        Act::Close
    } else {
        Act::Stay
    }
}

#[cfg(test)]
#[path = "config_views_tests.rs"]
mod tests;
