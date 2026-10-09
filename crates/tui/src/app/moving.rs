//! The frame clock's home in the app (`docs/tui.md`, "The working line"):
/// the one time every input sets, what the frame asked to wake for, and
/// whether motion stays still.
use std::time::Instant;

use crate::motion::Motion;

use super::App;

impl App {
    /// Sets the frame's time and the rail's wall time together, so
    /// elapsed times and animation agree. The loop calls it once per
    /// input, and once before the first frame.
    pub(crate) fn set_now(&mut self, now: Instant, wall_ms: u64) {
        self.motion.set_now(now, wall_ms);
        self.set_wall(wall_ms);
    }

    /// What moves on screen, and when it next moves.
    pub(crate) fn motion(&self) -> &Motion {
        &self.motion
    }

    /// The earliest wake the frame asked for, leaving none behind: what
    /// the loop arms the tick with after the draw.
    pub(crate) fn take_wake(&self) -> Option<Instant> {
        self.motion.take_wake()
    }

    /// Whether spinners, the glimmer and the pulse stay still. Home sets
    /// it from the launch description; the flat mode in Part 3 is the
    /// second source.
    pub(crate) fn set_reduced_motion(&mut self, on: bool) {
        self.motion.set_reduced(on);
    }
}
