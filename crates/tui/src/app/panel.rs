//! The session screen's side panel state: the attached session's folded
//! data behind a small interface (`docs/tui.md`, "The panel"). The app
//! calls [`App::panel_line`] for every attached-session line and reads the
//! fold through [`PanelState`]'s accessors; drawing lives in
//! `crate::view::panel`.

use std::collections::BTreeSet;

use contract::Envelope;
use contract::events::{
    ExtensionUi, McpServerFailed, McpServerReady, ModelChanged, PreambleBuilt, SessionStatus, Ui,
    UsageRecorded,
};

use super::App;

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
                        .filter(|span| *span > 0)
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

    /// Folds one attached-session envelope into the panel's data.
    pub(super) fn panel_line(&mut self, envelope: &Envelope) -> Vec<String> {
        self.panel_state.fold(envelope);
        Vec::new()
    }
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
