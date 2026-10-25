//! Home after a dropped connection (`docs/tui.md`, "A dropped
//! connection"): what the old connection held is gone, so home asks for
//! its feed and `recent` again and subscribes afresh. The rows, the stop
//! asks, the quit question's closes, the launch and the picker stay.

use super::super::App;

impl App {
    /// Clears the old connection's subscription state and open gate, and
    /// asks for the feed and the first `recent` page again: their new ids
    /// replace the old ones, so an old answer matches nothing. A delete is
    /// a hub command and is not resent, so each one waiting settles as
    /// unanswered: the row's note and a notice.
    pub(in crate::app) fn home_reset(&mut self) {
        let Some(home) = self.home.as_mut() else {
            return;
        };
        home.fed = false;
        home.subs.reset();
        // Every held total goes with the old connection, with the scoped
        // ids waiting and their dirty flags: a stale scoped answer
        // matches nothing afterwards. Rows stay for the refetch, which
        // the next outgoing pass queues into the home outbox.
        home.sessions.drop_all_totals();
        home.scoped_asks.clear();
        home.refetch_scoped = true;
        self.settle_deletes();
    }
}
