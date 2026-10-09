//! The narrow layout's rows below the conversation: the status line, the
//! widget row and the running delegates rows (`docs/tui.md`, "The narrow
//! layout", "Shedding"). Drawing lives in `crate::view::status_rows`.

use super::{App, Effect};

/// The conversation's least rows under the narrow layout's rows; picked,
/// not measured.
const MIN_ROWS: usize = 3;

/// How many delegates rows and status rows the narrow layout keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fit {
    /// The delegates rows kept.
    pub(crate) delegates: usize,
    /// The status rows kept.
    pub(crate) status: usize,
}

/// The rows kept on a `height`-row screen whose other rows below the
/// conversation take `base`, wanting `delegates` and `status` rows. The
/// delegates rows show only while the conversation keeps at least half
/// the screen; the status rows shed after them, keeping the conversation
/// its least rows.
pub(crate) fn fit(height: usize, base: usize, delegates: usize, status: usize) -> Fit {
    let conversation = height.saturating_sub(base + status + delegates);
    let delegates_kept = if 2 * conversation >= height {
        delegates
    } else {
        0
    };
    let status_kept = status.min(height.saturating_sub(base + delegates_kept + MIN_ROWS));
    Fit {
        delegates: delegates_kept,
        status: status_kept,
    }
}

/// The narrow layout's rows behind a small interface.
#[derive(Debug, Default)]
pub(crate) struct StatusRowsState {
    /// The widget row shows every line, not only its first.
    widget_open: bool,
}

impl App {
    /// The narrow layout's rows and how many of each fit; `None` outside
    /// it.
    pub(crate) fn narrow_fit(&self) -> Option<Fit> {
        let layout = self.chrome.layout().filter(|layout| layout.narrow)?;
        let width = layout.column.width;
        let height = usize::from(self.screen.height());
        let widgets = crate::view::status_rows::widget(self, width).len();
        let status = crate::view::status_rows::status(self, width).len();
        Some(fit(height, self.below_rows() + widgets, 0, status))
    }

    /// The rows the narrow layout adds below the conversation, given
    /// `base`: the widget rows and the status rows that fit. Zero outside
    /// the narrow layout.
    pub(super) fn narrow_rows(&self, base: usize) -> usize {
        let Some(layout) = self.chrome.layout().filter(|layout| layout.narrow) else {
            return 0;
        };
        let height = usize::from(self.screen.height());
        let width = layout.column.width;
        let widgets = crate::view::status_rows::widget(self, width).len();
        let status = crate::view::status_rows::status(self, width).len();
        widgets + fit(height, base + widgets, 0, status).status
    }

    /// Expands or collapses the narrow layout's widget row. The rows move,
    /// so the pages settle before the next frame.
    pub(super) fn toggle_widget_row(&mut self) -> Effect {
        self.status_rows.widget_open = !self.status_rows.widget_open;
        self.settle();
        Effect::None
    }

    /// Whether the widget row shows every line.
    pub(crate) fn widget_open(&self) -> bool {
        self.status_rows.widget_open
    }

    /// The attached session's spend so far, if any session is attached
    /// and home knows its row.
    pub(crate) fn attached_spend(&self) -> Option<f64> {
        self.attached_row().map(|row| row.spend)
    }
}

#[cfg(test)]
#[path = "status_rows_tests.rs"]
mod tests;
