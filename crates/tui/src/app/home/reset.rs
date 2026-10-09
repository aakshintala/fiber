//! Home after a dropped connection (`docs/tui.md`, "A dropped
//! connection"): what the old connection held is gone, so home asks for
//! its feed and `recent` again and subscribes afresh. The rows, the stop
//! asks, the quit question's closes, the launch and the picker stay.

use serde_json::{Map, Value};

use super::super::App;
use super::super::reconnect::UNANSWERED;
use super::{Ask, Subs};

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
        home.subs = Subs::default();
        home.opening = None;
        let mut deletes: Vec<String> = home
            .asks
            .iter()
            .filter(|(_, ask)| matches!(ask, Ask::Delete(_)))
            .map(|(id, _)| id.clone())
            .collect();
        deletes.sort();
        let payload =
            Map::from_iter([("message".to_owned(), Value::String(UNANSWERED.to_owned()))]);
        for id in deletes {
            if let Some(ask) = self.take_ask(&id) {
                self.answer_ask(ask, false, &payload);
            }
        }
    }
}
