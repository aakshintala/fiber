//! The reconnect banner: above the steering queue, and cut at the column.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::app::App;
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// A session line of `kind`.
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(S_A.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app attached at `width` by 12 in a turn with two steering rows,
/// whose connection dropped once.
fn dropped(width: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, 12);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    ));
    app.on_line(session_line(
        "steering_queue",
        json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
            {"content": [{"type": "text", "text": "and test it"}], "source": "driver", "command_id": "c_2"},
        ]}),
    ));
    app.disconnected();
    app.next_retry();
    app
}

/// The screen's text at `width` by 12.
fn screen(app: &App, width: u16) -> String {
    let area = Rect::new(0, 0, width, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn banner_above_the_steering_queue() {
    let app = dropped(60);
    insta::assert_snapshot!("banner_above_the_steering_queue", screen(&app, 60));
}

#[test]
fn banner_cut_at_the_column() {
    let app = dropped(24);
    insta::assert_snapshot!("banner_cut_at_the_column", screen(&app, 24));
}
