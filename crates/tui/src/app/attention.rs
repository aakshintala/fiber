//! The attention state on the app: what the hub's `attention` lines queued
//! (`docs/tui.md`, "Getting the person's attention").

use serde_json::{Map, Value};

use super::App;
use crate::Attention;

/// What the app holds for the hub's `attention` lines.
#[derive(Debug, Default)]
pub(super) struct State {
    /// Whether the terminal supports OSC 9, decided once at start.
    osc9: bool,
    /// The bytes the lines queued, written to the tty after the frame.
    out: Vec<u8>,
}

impl App {
    /// Records whether the terminal supports OSC 9, decided once at start
    /// from the environment (`docs/tui.md`, "Getting the person's
    /// attention").
    pub(crate) fn set_osc9(&mut self, osc9: bool) {
        self.attention.osc9 = osc9;
    }

    /// Folds one `attention` line's payload: its bytes queue for the tty
    /// after the frame (`docs/tui.md`, "Getting the person's attention").
    /// A line that does not parse queues nothing.
    pub(super) fn attention_line(&mut self, payload: &Map<String, Value>) {
        if let Some(line) = crate::attention::parse(payload) {
            let settings = self.attention_settings();
            let bytes = crate::attention::bytes(&line, settings, self.attention.osc9);
            self.attention.out.extend_from_slice(&bytes);
        }
    }

    /// Takes the attention bytes queued since the last frame.
    pub(crate) fn take_alerts(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.attention.out)
    }

    /// The attention settings from home's launch description, or the
    /// defaults without home state (`docs/tui.md`, "Getting the person's
    /// attention").
    fn attention_settings(&self) -> Attention {
        self.home
            .as_ref()
            .map(|home| home.launch.attention)
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
