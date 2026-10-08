//! The session screen's side panel state: the attached session's folded
//! data behind a small interface (`docs/tui.md`, "The panel"). The app
//! calls [`App::panel_line`] for every attached-session line and reads the
//! fold through [`PanelState`]'s accessors; drawing lives in
//! `crate::view::panel`.

use contract::Envelope;
use contract::events::{ExtensionUi, Ui};

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
}

impl PanelState {
    /// Folds one envelope of the attached session: an extension widget's
    /// latest lines replace it in place, empty lines remove it, and a new
    /// one goes last (`docs/tui.md`, "The panel").
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        if envelope.kind != "extension_ui" {
            return;
        }
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
