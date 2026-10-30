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
    /// unanswered: the row's note and a notice. Every project on home is
    /// asked again too: one scoped first page each, queued in the home
    /// outbox for the next outgoing pass beside the sessions request.
    /// Rows stay for those answers.
    pub(in crate::app) fn home_reset(&mut self) {
        let Some(home) = self.home.as_mut() else {
            return;
        };
        home.subs.reset();
        // Every held total goes with the old connection, with the scoped
        // ids waiting and their dirty flags: a stale scoped answer
        // matches nothing afterwards.
        home.sessions.drop_all_totals();
        let launch_project = home.launch_project().map(str::to_owned);
        let recent = home
            .recent
            .reset_for_connection(&home.sessions, launch_project.as_deref());
        for line in recent {
            home.subs.queue(line);
        }
        self.settle_deletes();
    }
}
