//! The frame clock's home in the app (`docs/tui.md`, "The working line"):
/// the one time every input sets, what the frame asked to wake for, and
/// whether motion stays still.
use std::time::Instant;

use crate::home::State;
use crate::motion::Motion;
use crate::working::Working;

use super::{App, Phase};

impl App {
    /// Sets the frame's time and the rail's wall time together, so
    /// elapsed times and animation agree. The loop calls it once per
    /// input, and once before the first frame.
    pub(crate) fn set_now(&mut self, now: Instant, wall_ms: u64) {
        // A changed animated line count requests one corrective frame;
        // recount after the previous frame has closed, before laying out.
        self.screen.pages_mut().recount_animated();
        self.screen.pages_mut().ext.wall_ms = Some(wall_ms);
        self.motion.set_now(now, wall_ms);
        self.set_wall(wall_ms);
    }

    /// What moves on screen, and when it next moves.
    pub(crate) fn motion(&self) -> &Motion {
        &self.motion
    }

    /// The earliest wake the frame asked for, leaving none behind: what
    /// the loop arms the tick with after the draw. The earliest of the
    /// motion wake, a one-shot layout correction, the reconciler's retry
    /// wake and the notices wake. Animation dues fold into motion first,
    /// as does a painted countdown's one-shot text-boundary ask.
    pub(crate) fn take_wake(&self) -> Option<Instant> {
        self.anim_asks();
        [
            self.motion.take_wake(),
            self.pages()
                .ext
                .cache
                .has_changed_pages()
                .then(|| self.motion.now_instant())
                .flatten(),
            self.items_wake(),
            self.rewind_wake(),
            self.notices.notices_wake(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Folds the painted animations' dues into the motion wake: each
    /// animated item on screen asks its next frame at its last tick plus
    /// its interval, and the loop arms the earliest ask after the frame.
    /// A notice that left the stack after its paint — fading in the
    /// post-draw sweep — contributes no due, so no animation timer is
    /// armed after fade or dismissal.
    fn anim_asks(&self) {
        let live = self.notices.live_ids();
        for (last, interval) in self.pages().ext.cache.anim_dues_live(&live) {
            self.motion.ask_animate(last, interval);
        }
    }

    /// Whether spinners, the glimmer and the pulse stay still. Home sets
    /// it from the launch description; the flat mode in Part 3 is the
    /// second source.
    pub(crate) fn set_reduced_motion(&mut self, on: bool) {
        self.motion.set_reduced(on);
    }

    /// What the working line says, while a turn runs: home is set, the
    /// turn is busy, the reconnect banner is not showing, and the feed
    /// row is not a live wait. A row that left with a stale waiting
    /// state still shows the line while the phase stays busy.
    pub(crate) fn working_line(&self) -> Option<Working> {
        let home = self.home.as_ref()?;
        let Phase::Attached {
            session,
            busy: true,
        } = &self.phase
        else {
            return None;
        };
        if self.banner().is_some() {
            return None;
        }
        let row = home.sessions.row(session)?;
        if row.left.is_none() && row.state == State::Waiting {
            return None;
        }
        // The working line follows the attached session, never the open
        // delegate's transcript.
        let pages = self.attached_screen().pages();
        Some(Working {
            started_ms: pages.running_started(),
            retry: pages.open_turn()?.pending_retry().cloned(),
        })
    }

    /// Whether the working line's row shows: the banner's, or the
    /// working line's. The conversation keeps one row for either.
    pub(crate) fn working_row_shown(&self) -> bool {
        self.banner().is_some() || self.working_line().is_some()
    }

    /// Home's row with `key`, live or exited; what the drawn home rows
    /// ask their spin for.
    pub(crate) fn row_by_key(&self, key: u64) -> Option<&crate::home::Row> {
        self.home.as_ref()?.sessions.by_key(key)
    }
}

#[cfg(test)]
#[path = "moving_tests.rs"]
mod tests;
