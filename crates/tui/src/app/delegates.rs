//! The panel's Delegates card on the app side: its wheel (`docs/tui.md`,
//! "The panel": the card "scrolls on its own under the mouse wheel").

use super::App;

impl App {
    /// The wheel over the panel at screen `row`: the Delegates card's rows
    /// scroll the card, any other row the panel. The row maps to a panel
    /// row as the panel's draw places them: the first on the rect's second
    /// row, after the panel's clamped scroll.
    pub(super) fn wheel_panel(&mut self, row: u16, up: bool) {
        let Some(panel) = self.chrome().layout().and_then(|layout| layout.panel) else {
            return;
        };
        let (rows, span) = crate::view::panel::rows_and_delegates(self, panel.width);
        let height = usize::from(panel.height.saturating_sub(1));
        let skip = self
            .panel_state
            .scroll()
            .min(rows.len().saturating_sub(height));
        let over_card = row
            .checked_sub(panel.y.saturating_add(1))
            .map(|at| usize::from(at).saturating_add(skip))
            .zip(span)
            .is_some_and(|(at, span)| span.contains(&at));
        if over_card {
            self.panel_state.scroll_delegates(up);
        } else {
            self.scroll_panel(up);
        }
    }
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;
