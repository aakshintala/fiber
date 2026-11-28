//! Scaffolding shared by search results and chrome tests (`docs/tui.md`,
//! "Search" and "Layout"): an attached app, a reply, and the injected
//! clock's now, plus shared layout fixtures.

use std::path::PathBuf;
use std::time::Instant;

use contract::clock::Clock;
use contract::{HubEventKind, events::EventKind};
use serde_json::{Value, json};

use crate::app::App;
use crate::link::Line;

/// The shared rail, conversation, input and panel layout used by chrome tests.
pub(crate) fn four_region_layout(
    conversation_height: i64,
) -> Vec<(crate::layout::Region, crate::ext::RawRect)> {
    [
        (
            crate::layout::Region::Rail,
            crate::ext::RawRect {
                x: 0,
                y: 0,
                width: 20,
                height: 40,
            },
        ),
        (
            crate::layout::Region::Conversation,
            crate::ext::RawRect {
                x: 30,
                y: 0,
                width: 100,
                height: conversation_height,
            },
        ),
        (
            crate::layout::Region::InputBox,
            crate::ext::RawRect {
                x: 30,
                y: 35,
                width: 100,
                height: 5,
            },
        ),
        (
            crate::layout::Region::Panel,
            crate::ext::RawRect {
                x: 140,
                y: 0,
                width: 20,
                height: 40,
            },
        ),
    ]
    .into()
}

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
            kind: EventKind::from_name(kind),
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
    app.on_line(Line::Hub(contract::HubLine {
        kind: HubEventKind::HubHello,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
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
        kind: EventKind::TextCompleted,
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
