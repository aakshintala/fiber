//! Tests for the notice stack, its boxes and overlays.

use super::{NoticeBox, Notices};
use crate::app::App;
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// Notices of `texts`, oldest first.
fn stack(texts: &[&str]) -> Notices {
    let mut notices = Notices::default();
    for text in texts {
        notices.push((*text).to_owned());
    }
    notices
}

/// Each box's rows, trailing spaces trimmed.
fn trimmed(boxes: &[NoticeBox]) -> Vec<Vec<String>> {
    boxes
        .iter()
        .map(|b| b.rows.iter().map(|row| row.trim_end().to_owned()).collect())
        .collect()
}

#[test]
fn newest_on_top_three_then_more() {
    let notices = stack(&["one", "two", "three", "four"]);
    let boxes = notices.boxes(100);
    assert_eq!(
        trimmed(&boxes),
        [
            vec!["four                                   ✕"],
            vec!["three                                  ✕"],
            vec!["two                                    ✕"],
            vec!["+1 more"],
        ]
    );
    assert_eq!(
        boxes.iter().map(|b| b.id).collect::<Vec<_>>(),
        [Some(3), Some(2), Some(1), None]
    );
    // Every row is the box's width.
    for row in boxes.iter().flat_map(|b| b.rows.iter()) {
        assert_eq!(crate::format::width(row), 40, "{row:?}");
    }
    assert_eq!(stack(&["a", "b", "c"]).boxes(100).len(), 3);
    let five = stack(&["a", "b", "c", "d", "e"]).boxes(100);
    assert_eq!(five.len(), 4);
    assert_eq!(five[3].rows[0].trim_end(), "+2 more");
    assert!(Notices::default().boxes(100).is_empty());
}

#[test]
fn a_box_is_two_fifths_of_the_width_up_to_sixty_columns() {
    let notices = stack(&["hi"]);
    let wide = |columns| crate::format::width(&notices.boxes(columns)[0].rows[0]);
    assert_eq!(wide(100), 40);
    assert_eq!(wide(149), 59);
    assert_eq!(wide(150), 60);
    assert_eq!(wide(200), 60);
    assert_eq!(wide(12), 4);
}

#[test]
fn a_long_notice_wraps_to_three_lines_ending_more() {
    // 40% of 30 columns is 12: ten of text, a space and the ✕.
    let notices = stack(&["aaaa bbbb cccc dddd eeee ffff"]);
    assert_eq!(
        trimmed(&notices.boxes(30)),
        [vec!["aaaa bbbb  ✕", "cccc dddd", "eeee ffff"]]
    );
    let notices = stack(&["aaaa bbbb cccc dddd eeee ffff gggg"]);
    assert_eq!(
        trimmed(&notices.boxes(30)),
        [vec!["aaaa bbbb  ✕", "cccc dddd", "eee … more"]]
    );
}

#[test]
fn narrow_widths_draw_what_fits_and_never_panic() {
    let notices = stack(&["a notice that is long enough to wrap", "b", "c", "d"]);
    for columns in 0..12 {
        for b in notices.boxes(columns) {
            for row in &b.rows {
                assert!(
                    crate::format::width(row) <= usize::from(columns),
                    "{columns}"
                );
            }
        }
    }
    assert!(notices.boxes(0).is_empty());
    assert!(notices.boxes(7).is_empty());
    assert!(!notices.boxes(8).is_empty());
}

#[test]
fn dismiss_and_the_overlays() {
    let mut notices = stack(&["one", "two", "three", "four"]);
    assert_eq!(notices.overlay(), None);
    notices.open(1);
    assert_eq!(notices.overlay(), Some(vec!["two".to_owned()]));
    notices.dismiss(1);
    assert_eq!(notices.overlay(), None);
    assert_eq!(
        trimmed(&notices.boxes(100))
            .iter()
            .map(|rows| rows[0].trim_end_matches(['✕', ' ']).to_owned())
            .collect::<Vec<_>>(),
        ["four", "three", "one"]
    );
    // Opening a notice that is gone does nothing.
    notices.open(1);
    assert_eq!(notices.overlay(), None);
    notices.open_all();
    assert_eq!(
        notices.overlay(),
        Some(vec![
            "four".to_owned(),
            "three".to_owned(),
            "one".to_owned()
        ])
    );
    // Dismissing another notice keeps the list open.
    notices.dismiss(0);
    assert_eq!(
        notices.overlay(),
        Some(vec!["four".to_owned(), "three".to_owned()])
    );
    assert!(notices.close());
    assert!(!notices.close());
    let mut empty = Notices::default();
    empty.open_all();
    assert_eq!(empty.overlay(), None);
}

/// An app attached to [`S_A`], connected, 100 columns wide.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(100, 24);
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::default(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// A `notice` line for `session`.
fn notice(session: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "notice".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"code": "extension_failed", "message": message})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

#[test]
fn the_sessions_notices_and_the_terminals_own_share_the_stack() {
    let mut app = attached();
    app.on_line(notice(S_A, "An extension failed to load."));
    app.on_line(notice("s_bbbbbbbbbbbbbbbb", "Not ours."));
    app.on_key(Key::AltA, fakes::clock::FakeClock::new().now());
    app.disconnected();
    let firsts: Vec<String> = app
        .notices()
        .iter()
        .map(|b| b.rows[0].trim_end_matches(['✕', ' ']).to_owned())
        .collect();
    assert_eq!(
        firsts,
        [
            "Connection lost.",
            "No requests waiting.",
            "An extension failed to load."
        ]
    );
}

#[test]
fn the_app_dismisses_and_opens_notices_and_esc_closes_the_overlay() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached();
    for n in 0..4 {
        app.on_line(notice(S_A, &format!("notice {n}")));
    }
    app.open_notice(2);
    assert_eq!(app.notice_overlay(), Some(vec!["notice 2".to_owned()]));
    assert_eq!(app.on_key(Key::Esc, now), crate::app::Effect::None);
    assert_eq!(app.notice_overlay(), None);
    app.open_more_notices();
    assert_eq!(app.notice_overlay().map(|all| all.len()), Some(4));
    app.on_key(Key::Esc, now);
    app.dismiss_notice(3);
    assert_eq!(app.notice(), Some("notice 2"));
    assert_eq!(app.notices().len(), 3);
}
