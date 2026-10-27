//! The app tests' shared fixtures: the `hub_hello` line and the injected
//! clock's now. Fixtures never sleep, read the wall clock, or advance a
//! clock; a test advances a fake clock only after a signal that the code
//! under test is waiting on it.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;

use super::{App, Effect};
use crate::home::Launch;
use crate::link::Line;

/// The session every app test attaches to.
pub(crate) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// A `hub_hello` this terminal reads.
pub(crate) fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// The injected clock's now.
pub(crate) fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app on home at `w` by `h`, launched with `launch`: home first, then
/// the size, the order every home test already builds.
pub(crate) fn home_with(w: u16, h: u16, launch: Launch) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch);
    app.set_size(w, h);
    app
}

/// An app attached to `SESSION` at `width` by `height`: new, then the
/// size, then attach, the order every attach test already builds.
pub(crate) fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// One session envelope.
pub(crate) fn session_line(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Every line an effect sends, parsed.
pub(crate) fn sent(effect: Effect) -> Vec<serde_json::Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. } => Vec::new(),
    }
}
