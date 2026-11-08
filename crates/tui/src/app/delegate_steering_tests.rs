//! A fiber delegate's view draws and edits its own steering queue through
//! the same card, keys and targets as the conversation (`docs/tui.md`,
//! "Swapped views" and "Steering").

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::items::testkit::{
    DELEGATE_A, commands, home, opened, opened_item, session_line, start_delegate,
};
use super::{App, Effect};
use crate::keys::Key;
use crate::link::Line;

/// A `steering_queue` line for `session`: each row its text and its
/// `steer` id, `None` for Fiber's own message.
fn queue_line(session: &str, rows: &[(&str, Option<&str>)]) -> Line {
    let messages: Vec<Value> = rows
        .iter()
        .map(|(text, id)| {
            let mut message = json!({
                "content": [{"type": "text", "text": text}],
                "source": "driver",
            });
            if let (Some(id), Some(object)) = (id, message.as_object_mut()) {
                object.insert("command_id".to_owned(), json!(id));
            }
            message
        })
        .collect();
    let Line::Session(mut envelope) = session_line("steering_queue", json!({"messages": messages}))
    else {
        panic!("a session line")
    };
    envelope.session_id = contract::SessionId(session.to_owned());
    Line::Session(envelope)
}

/// Types `text` one key at a time.
fn type_text(app: &mut App, clock: &fakes::clock::FakeClock, text: &str) {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), clock.now());
    }
}

/// The lines `effect` sends.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => commands(lines),
        other => panic!("expected a send, got {other:?}"),
    }
}

/// The whole app drawn at 80x24, rows trimmed of trailing spaces.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn a_steer_typed_in_a_delegate_view_shows_in_its_steering_card() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    type_text(&mut app, &clock, "use the parser");
    let out = sent(app.on_key(Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
    let id = out[0]["id"].as_str().unwrap_or_default().to_owned();
    app.on_line(queue_line(DELEGATE_A, &[("use the parser", Some(&id))]));
    let shown = screen(&app);
    assert!(shown.contains("Steering"), "{shown}");
    assert!(shown.contains("↳ use the parser"), "{shown}");
    insta::assert_snapshot!("queued_delegate_steering", shown);
}
