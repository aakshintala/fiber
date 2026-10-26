//! The app tests' shared fixtures: the `hub_hello` line and the injected
//! clock's now. Fixtures never sleep, read the wall clock, or advance a
//! clock; a test advances a fake clock only after a signal that the code
//! under test is waiting on it.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;

use super::App;
use crate::home::Launch;
use crate::link::Line;

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
