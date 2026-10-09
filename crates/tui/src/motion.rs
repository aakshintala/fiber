//! The frame clock (`docs/tui.md`, "The working line"): what moves on the
//! tick, and when the next tick is asked for. Builders read the frame
//! through the pure calls, which ask for nothing; only the draw site that
//! puts a moving thing on screen asks for the next wake, and the loop
//! arms the tick with the earliest ask after the frame.

use std::cell::Cell;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;

use crate::tick::TICK;

/// The spinner's ten frames, one per tick.
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// The spinner's still form: what shows under reduced motion and with no
/// time set (`docs/tui.md`, "State glyphs").
pub(crate) const STILL: &str = "●";

/// What moves on screen, and when it next moves. The epoch is the first
/// `now` given; a frame is one [`TICK`] from it.
#[derive(Default)]
pub(crate) struct Motion {
    reduced: bool,
    epoch: Option<Instant>,
    now: Option<(Instant, u64)>,
    wake: Cell<Option<Instant>>,
}

impl Motion {
    /// Sets the frame's time and the rail's wall time together: one call
    /// per input, so elapsed times and animation agree.
    pub(crate) fn set_now(&mut self, now: Instant, wall_ms: u64) {
        if self.epoch.is_none() {
            self.epoch = Some(now);
        }
        self.now = Some((now, wall_ms));
    }

    /// Whether spinners, the glimmer and the pulse stay still
    /// (`docs/tui.md`, "The working line").
    pub(crate) fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    /// This frame's spinner: still under reduced motion and with no time
    /// set. Reads the frame and asks for nothing.
    pub(crate) fn spinner(&self) -> &'static str {
        if self.reduced {
            return STILL;
        }
        let Some(frame) = self.frame() else {
            return STILL;
        };
        // Ten frames: the index is always in range.
        SPINNER.get(usize::try_from(frame % 10).unwrap_or(0)).copied().unwrap_or(STILL)
    }

    /// Asks for the next frame's boundary. Nothing under reduced motion
    /// or with no time set.
    pub(crate) fn ask_frame(&self) {
        let (Some(epoch), Some((now, _))) = (self.epoch, self.now) else {
            return;
        };
        if self.reduced {
            return;
        }
        let frame = now.saturating_duration_since(epoch).as_millis() / TICK.as_millis();
        let step =
            u32::try_from(frame.saturating_add(1).min(u128::from(u32::MAX))).unwrap_or(u32::MAX);
        if let Some(at) = epoch.checked_add(TICK.checked_mul(step).unwrap_or(Duration::MAX)) {
            self.ask(at);
        }
    }

    /// Writes the spinner into one cell, keeping that cell's style, and
    /// asks for the next frame. With nothing moving the mark stays as
    /// drawn, so a still screen draws no frames.
    pub(crate) fn spin(&self, buf: &mut Buffer, x: u16, y: u16) {
        if self.reduced || self.now.is_none() {
            return;
        }
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(self.spinner());
        }
        self.ask_frame();
    }

    /// Drops the frame's asks: the next render asks them again.
    pub(crate) fn clear_wake(&self) {
        self.wake.set(None);
    }

    /// The earliest ask the frame made, leaving none behind.
    pub(crate) fn take_wake(&self) -> Option<Instant> {
        self.wake.take()
    }

    /// This frame's number, from the epoch; none with no time set.
    fn frame(&self) -> Option<u64> {
        let (Some(epoch), Some((now, _))) = (self.epoch, self.now) else {
            return None;
        };
        let millis = now.saturating_duration_since(epoch).as_millis();
        u64::try_from(millis / TICK.as_millis()).ok()
    }

    /// Keeps the earliest ask: the loop arms the tick with it.
    fn ask(&self, at: Instant) {
        let earlier = self.wake.get().is_none_or(|wake| at < wake);
        if earlier {
            self.wake.set(Some(at));
        }
    }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
