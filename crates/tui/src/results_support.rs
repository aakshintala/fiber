//! Scaffolding shared by the search results tests (`docs/tui.md`,
//! "Search"): an attached app with a running turn, a reply, and the
//! injected clock's now.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::app::App;
use crate::link::Line;

/// The session the scaffolding attaches to.
pub(crate) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
pub(crate) fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// An app connected to the hub and attached to [`SESSION`] at `width` by
/// `height`, with a running turn: the conversation is the whole screen.
pub(crate) fn attached(width: u16, height: u16) -> App {
    let session = |kind: &str, payload: Value, action: Option<&str>| {
        Line::Session(contract::Envelope {
            kind: kind.to_owned(),
            session_id: contract::SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: None,
            payload: payload.as_object().cloned().unwrap_or_default(),
        })
    };
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    assert!(
        app.on_line(Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }))
        .is_empty()
    );
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(session(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app
}

/// A reply `text` from message `action`: completing a streamed message
/// replaces its line, otherwise it appends.
pub(crate) fn reply(app: &mut App, action: &str, text: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "text_completed".to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: json!({ "text": text })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
}
