//! The frame clock (`docs/tui.md`, "The working line"): what moves on the
//! tick, and when the next tick is asked for. Builders read the frame
//! through the pure calls, which ask for nothing; only the draw site that
//! puts a moving thing on screen asks for the next wake, and the loop
//! arms the tick with the earliest ask after the frame.

use std::cell::Cell;
use std::ops::Range;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;

use crate::home::{self, Row};
use crate::tick::TICK;

/// The spinner's ten frames, one per tick.
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// The spinner's still form: what shows under reduced motion and with no
/// time set (`docs/tui.md`, "State glyphs").
pub(crate) const STILL: &str = "●";
/// How long a waiting card pulses from its state's start, in
/// milliseconds (`docs/tui.md`, "The rail").
pub(crate) const PULSE_MS: u64 = 10_000;

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
        SPINNER
            .get(usize::try_from(frame % 10).unwrap_or(0))
            .copied()
            .unwrap_or(STILL)
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

    /// This frame's glyph for `row`: the spinner while its session
    /// works, else the still glyph. Reads the frame and asks for
    /// nothing.
    pub(crate) fn glyph(&self, row: &Row) -> &'static str {
        if Self::spins(row) {
            self.spinner()
        } else {
            home::glyph(row)
        }
    }

    /// Whether `row`'s glyph moves: a live row in a working state.
    pub(crate) fn spins(row: &Row) -> bool {
        row.left.is_none() && matches!(row.state, home::State::Working | home::State::Retrying)
    }

    /// Asks for the next frame when `row`'s glyph moves. Nothing under
    /// reduced motion or with no time set.
    pub(crate) fn ask_spin(&self, row: &Row) {
        if Self::spins(row) {
            self.ask_frame();
        }
    }

    /// Whether `row`'s waiting pulse shows, and bright: ten seconds from
    /// `since_ms`, bright on every other four frames. A `since` after now
    /// counts as no wait. Reads the frame and asks for nothing.
    pub(crate) fn pulse(&self, since_ms: u64) -> Option<bool> {
        if self.reduced {
            return None;
        }
        let (Some(frame), Some((_, wall))) = (self.frame(), self.now) else {
            return None;
        };
        if wall.saturating_sub(since_ms) >= PULSE_MS {
            return None;
        }
        Some(frame / 4 % 2 == 0)
    }

    /// Asks for the pulse's next change while `row` waits: the next
    /// four-frame boundary, and the pulse's end. Nothing once it holds
    /// still.
    pub(crate) fn ask_pulse(&self, row: &Row) {
        let Some((now, wall)) = self.now else {
            return;
        };
        if self.reduced {
            return;
        }
        let Some(since) = pulse_since(row) else {
            return;
        };
        if wall.saturating_sub(since) >= PULSE_MS {
            return;
        }
        let Some(epoch) = self.epoch else {
            return;
        };
        let frame = now.saturating_duration_since(epoch).as_millis() / TICK.as_millis();
        // The next four-frame boundary past this one.
        let next = frame / 4 * 4;
        let step = u32::try_from(next.saturating_add(4)).unwrap_or(u32::MAX);
        if let Some(at) = epoch.checked_add(TICK.checked_mul(step).unwrap_or(Duration::MAX)) {
            self.ask(at);
        }
        let end = since.saturating_add(PULSE_MS);
        let at = now
            .checked_add(Duration::from_millis(end.saturating_sub(wall)))
            .unwrap_or(now);
        self.ask(at);
    }

    /// The frame's wall time, in milliseconds since the epoch; none with
    /// no time set.
    pub(crate) fn wall_ms(&self) -> Option<u64> {
        self.now.map(|(_, wall)| wall)
    }

    /// The glimmer's cells in a word `len` cells wide: a 3-cell band
    /// sweeping one cell a frame from two cells before the word, then
    /// resting (`docs/tui.md`, "The working line"). Reads the frame and
    /// asks for nothing.
    pub(crate) fn glimmer(&self, len: usize) -> Option<Range<usize>> {
        if self.reduced {
            return None;
        }
        let frame = self.frame()?;
        // Nine sweeping frames, then eight resting: a period of about
        // two seconds.
        let at = frame % 17;
        if at >= 9 {
            return None;
        }
        let from = usize::try_from(at.saturating_sub(2)).unwrap_or(0);
        let end = usize::try_from(at.saturating_add(1)).unwrap_or(usize::MAX);
        let end = end.min(len);
        (from < end).then_some(from..end)
    }

    /// Asks for a wall-clock moment `ms`: the frame converts it against
    /// its own wall time, or asks at once when it already passed. Asks
    /// even under reduced motion: the time and the countdown keep a
    /// one-second wake. Nothing with no time set.
    pub(crate) fn ask_wall(&self, ms: u64) {
        let Some((now, wall)) = self.now else {
            return;
        };
        let at = now
            .checked_add(Duration::from_millis(ms.saturating_sub(wall)))
            .unwrap_or(now);
        self.ask(at);
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
        // Equal asks name the same observable wake, so only the minimum matters.
        self.wake
            .set(Some(self.wake.get().map_or(at, |wake| wake.min(at))));
    }
}

/// A live waiting row's pulse start: its status's `since`. Rows that
/// left, rows not waiting and rows with no status hold still.
fn pulse_since(row: &Row) -> Option<u64> {
    if row.left.is_none() && row.state == home::State::Waiting {
        row.status.as_ref().map(|status| status.since)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
