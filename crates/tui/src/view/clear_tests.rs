//! The clear overlay's drawing: the title, the dim wait line, the
//! choices with Enter's focused, and their clicks.

use super::draw;
use crate::app::{App, Effect};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::path::PathBuf;

/// An app on home at `width` by `height`, attached to `session`.
fn home(width: u16, height: u16, session: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(width, height);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(session.to_owned()));
    app
}

/// A `session_status` for `session` waiting on an approval.
fn waiting(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "review #688", "workspace": "/w", "project": "-w",
            "state": "waiting",
            "waiting": {"request_id": "r_1", "kind": "approval",
                "summary": "npm test"},
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Types `text`, then Enter.
fn enter(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        let _ = app.on_key(Key::Char(ch), now);
    }
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
}

/// Renders `app` at `width` by `height` as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// The drawn rows of `buf`, and whether each non-blank cell is `modifier`.
fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

#[test]
fn clear_names_the_approval_wait_with_enter_barred() {
    let mut app = home(80, 24, "s_aaaaaaaaaaaaaaaa");
    app.on_line(waiting("s_aaaaaaaaaaaaaaaa"));
    enter(&mut app, "/clear");
    assert!(app.clear_open());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    let shown = crate::view::text(&buf);
    assert!(shown.contains("Clear"), "{shown}");
    assert!(
        shown.contains("review #688 waits on an approval."),
        "{shown}"
    );
    assert!(shown.contains("clear anyway"), "{shown}");
    assert!(shown.contains("stay"), "{shown}");
    assert!(shown.contains("click a choice"), "{shown}");
    // The wait line stays dim.
    let wait = (0..24)
        .map(|y| row_text(&buf, y, 80))
        .find(|row| row.contains("waits on an approval"))
        .expect("the wait line draws");
    let y = (0..24)
        .find(|y| row_text(&buf, *y, 80).contains("waits on an approval"))
        .expect("the wait line draws");
    assert!(
        (0..80)
            .filter(|x| buf[(*x, y)].symbol() != " ")
            .all(|x| buf[(x, y)].modifier.contains(Modifier::DIM)),
        "{wait}"
    );
    // Exactly one row keeps the bar: Enter's, reading bold; the other
    // choice reads dim.
    let mut barred = 0;
    for y in 0..24 {
        let row = row_text(&buf, y, 80);
        if row.contains("› ") {
            barred += 1;
            assert!(row.contains("Enter"), "{row}");
            assert!(row.contains("clear anyway"), "{row}");
            assert!(
                (0..80)
                    .filter(|x| buf[(*x, y)].symbol() != " ")
                    .all(|x| buf[(x, y)].modifier.contains(Modifier::BOLD)),
                "{row}"
            );
        }
        if row.contains("Esc") && row.contains("stay") {
            assert!(!row.contains("› "), "{row}");
            let at = row.find("stay").expect("the stay label");
            assert!(
                (u16::try_from(at).unwrap_or(u16::MAX)
                    ..u16::try_from(at + "stay".len()).unwrap_or(0))
                    .all(|x| buf[(x, y)].modifier.contains(Modifier::DIM)),
                "{row}"
            );
        }
    }
    assert_eq!(barred, 1);
}

#[test]
fn clear_draws_over_the_session_screen() {
    let mut app = home(160, 48, "s_aaaaaaaaaaaaaaaa");
    app.on_line(waiting("s_aaaaaaaaaaaaaaaa"));
    enter(&mut app, "/clear");
    let shown = screen(&app, 160, 48);
    assert!(shown.contains("Clear"), "{shown}");
    assert!(
        shown.contains("review #688 waits on an approval."),
        "{shown}"
    );
}
