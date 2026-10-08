//! Tests for bare URLs (`docs/tui.md`, "Links").

use super::urls;

/// The URLs `text` holds, as text.
fn found(text: &str) -> Vec<String> {
    urls(text)
        .into_iter()
        .map(|range| text.get(range).unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn urls_finds_bare_urls_and_cuts_trailing_punctuation() {
    type Case<'a> = (&'a str, &'a str, Vec<&'a str>);
    let cases: Vec<Case<'_>> = vec![
        (
            "a bare URL",
            "see http://example.com/a here",
            vec!["http://example.com/a"],
        ),
        (
            "an https URL",
            "see https://example.com",
            vec!["https://example.com"],
        ),
        (
            "a trailing period",
            "see http://example.com.",
            vec!["http://example.com"],
        ),
        (
            "trailing punctuation",
            "see http://example.com,;:",
            vec!["http://example.com"],
        ),
        (
            "a quoted URL",
            "\"http://example.com/a\"",
            vec!["http://example.com/a"],
        ),
        (
            "a parenthesised URL",
            "(http://example.com/a)",
            vec!["http://example.com/a"],
        ),
        (
            "a URL with a balanced paren",
            "http://example.com/a(b)",
            vec!["http://example.com/a(b)"],
        ),
        (
            "a URL followed by a close paren it did not open",
            "see (http://example.com/a),",
            vec!["http://example.com/a"],
        ),
        (
            "a bracketed URL",
            "[http://example.com/a]",
            vec!["http://example.com/a"],
        ),
        ("http alone is none", "see http:// here", vec![]),
        (
            "a URL inside a word",
            "abhttp://example.com/cd",
            vec!["http://example.com/cd"],
        ),
        (
            "two URLs",
            "http://a.example and https://b.example/c",
            vec!["http://a.example", "https://b.example/c"],
        ),
        (
            "a long URL that wraps on screen",
            "see http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh for more",
            vec!["http://example.com/aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh"],
        ),
    ];
    for (name, text, expected) in cases {
        let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
        assert_eq!(found(text), expected, "{name}");
    }
}

// Click, focus and stale targets for links below.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::link_hash;
use crate::app::{App, Effect};
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{TargetId, hit};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// A session line.
fn line(kind: &str, payload: Value, action: Option<&str>) -> Line {
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
}

/// The hub's `hub_hello`.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// An app attached at `width` by `height`, showing one done turn with
/// `text`.
fn replied(width: u16, height: u16, text: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    assert!(app.on_line(hello()).is_empty());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_line(line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": " "}]}]}),
        None,
    ));
    app.on_line(line("text_completed", json!({ "text": text }), Some("a_m")));
    app.on_line(line(
        "turn_completed",
        json!({"outcome": "completed"}),
        None,
    ));
    app
}

/// The app drawn at its size, with its targets.
fn draw(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let (width, height) = (app.screen.width(), app.screen.height());
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (buf, targets)
}

/// The link targets drawn now.
fn links(app: &App) -> Vec<(TargetId, Rect)> {
    let (_, targets) = draw(app);
    targets
        .into_iter()
        .filter(|target| matches!(target.id, TargetId::Link { .. }))
        .map(|target| (target.id, target.rect))
        .collect()
}

/// The first link target whose URL is `url`.
fn link_for(app: &App, url: &str) -> (TargetId, Rect) {
    let area = app.conversation_area();
    app.visible_links(area)
        .into_iter()
        .find(|link| link.url == url)
        .map(|link| (link.id, link.rects.into_iter().next().unwrap_or_default()))
        .unwrap_or_else(|| panic!("no link to {url}"))
}

#[test]
fn a_bare_url_inside_a_markdown_label_keeps_the_markdown_destination() {
    let mut app = replied(
        60,
        12,
        "[https://label.example](https://destination.example)",
    );
    app.set_opener(true);
    let area = app.conversation_area();
    let found = app.visible_links(area);
    // One target, not two: the label's cells keep the markdown
    // destination, so the target under the pointer opens it.
    assert_eq!(found.len(), 1, "{found:?}");
    let rect = found[0].rects.first().expect("a link rect");
    let (_, targets) = draw(&app);
    let id = hit(&targets, rect.x, rect.y).expect("a link under the label");
    assert_eq!(id, found[0].id);
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://destination.example".to_owned())
    );
}

#[test]
fn a_click_on_a_markdown_link_opens_its_destination() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
    assert!(!app.copied());
}

#[test]
fn a_click_on_a_bare_url_opens_it() {
    let mut app = replied(60, 12, "see http://example.com/a here");
    app.set_opener(true);
    let (id, _) = link_for(&app, "http://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("http://example.com/a".to_owned())
    );
}

#[test]
fn a_link_wrapped_over_two_rows_is_one_target_on_both() {
    let mut app = replied(16, 12, "[a long docs link here](http://example.com/a) tail");
    app.set_opener(true);
    let area = app.conversation_area();
    let found = app.visible_links(area);
    let entry = found
        .iter()
        .find(|link| link.url == "http://example.com/a")
        .unwrap_or_else(|| panic!("no wrapped link in {found:?}"));
    assert_eq!(entry.rects.len(), 2, "{entry:?}");
    assert_eq!(entry.rects[0].y + 1, entry.rects[1].y);
    // Either row's target opens the same destination.
    for rect in &entry.rects {
        assert_eq!(
            app.on_click(entry.id),
            Effect::OpenLink("http://example.com/a".to_owned()),
            "{rect:?}"
        );
    }
}

#[test]
fn with_no_opener_a_click_copies_and_shows_copied() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(false);
    let (id, _) = link_for(&app, "https://example.com/a");
    assert_eq!(
        app.on_click(id),
        Effect::Copy("https://example.com/a".to_owned())
    );
    assert!(app.copied());
}

#[test]
fn a_click_on_a_stale_link_target_does_nothing() {
    let text: Vec<String> = (0..30).map(|n| format!("paragraph {n}")).collect();
    let mut app = replied(
        40,
        12,
        &format!("{}\n\n[docs](https://example.com/a)", text.join("\n\n")),
    );
    app.set_opener(true);
    let (id, rect) = link_for(&app, "https://example.com/a");
    // PageUp in the same read as the click: the frame the click was drawn
    // from is stale, so the old id opens nothing.
    app.on_key(Key::PageUp, now());
    assert_eq!(app.on_click(id), Effect::None, "stale {rect:?}");
}

#[test]
fn a_stale_id_names_its_row_column_and_hash() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    let TargetId::Link { row, col, url } = id else {
        panic!("a link id");
    };
    // Same cell, another URL.
    let other = TargetId::Link {
        row,
        col,
        url: link_hash("https://other.example"),
    };
    assert_eq!(app.on_click(other), Effect::None);
    // Same URL, another cell.
    let moved = TargetId::Link {
        row: row.saturating_add(1),
        col,
        url,
    };
    assert_eq!(app.on_click(moved), Effect::None);
    // The drawn id still opens.
    assert_eq!(
        app.on_click(id),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn y_on_a_focused_link_copies_its_url() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("https://example.com/a".to_owned())
    );
    assert!(app.copied());
}

#[test]
fn enter_on_a_focused_link_opens_it() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.set_opener(true);
    let (id, _) = link_for(&app, "https://example.com/a");
    app.focus = Some(id);
    assert_eq!(
        app.on_key(Key::Enter, now()),
        Effect::OpenLink("https://example.com/a".to_owned())
    );
}

#[test]
fn a_link_under_the_notice_overlay_is_not_a_target() {
    let mut app = replied(60, 12, "[docs](https://example.com/a)");
    app.on_line(line(
        "notice",
        json!({"code": "extension_failed", "message": "careful"}),
        None,
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    assert!(links(&app).is_empty());
}

#[test]
fn a_press_on_a_link_still_starts_a_selection() {
    let mut app = replied(60, 12, "[docs](https://example.com/a) tail");
    let (_, targets) = draw(&app);
    let link = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Link { .. }))
        .expect("a link target");
    let at = (link.rect.x, link.rect.y);
    let report = |app: &mut App, kind: MouseKind, (col, row): (u16, u16)| {
        let (_, targets) = draw(app);
        app.on_select(&Mouse { kind, col, row }, &targets)
    };
    assert_eq!(
        report(&mut app, MouseKind::Press(Button::Left), at),
        Effect::None
    );
    let to = (at.0.saturating_add(6), at.1);
    assert_eq!(
        report(&mut app, MouseKind::Drag(Button::Left), to),
        Effect::None
    );
    assert!(app.select.span().is_some());
    assert!(matches!(
        report(&mut app, MouseKind::Release, to),
        Effect::Copy(_)
    ));
}
