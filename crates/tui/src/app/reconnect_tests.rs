//! Tests for the dropped connection: the backoff, the banner and its
//! attempt number, and which failure is a notice (`docs/tui.md`, "A
//! dropped connection").

use std::path::PathBuf;
use std::time::Duration;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::{App, Effect, Link};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        version: "0.0.1".to_owned(),
        logo_glyph: "⌇".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` at `schema`.
fn hello_at(schema: u32) -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: schema,
        payload: serde_json::Map::new(),
    })
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    hello_at(contract::SCHEMA_VERSION)
}

/// A `hub_hello` for a schema newer than this terminal reads.
fn newer() -> Line {
    hello_at(contract::SCHEMA_VERSION + 1)
}

/// The banner for attempt `n`.
fn banner(n: u64) -> Option<String> {
    Some(format!("Connection lost · reconnecting (attempt {n})…"))
}

/// One failed connect, as the loop handles it: the notice, then the
/// permit's delay.
fn failed_connect(app: &mut App, error: &str) -> Option<Duration> {
    app.connect_failed(format!("Could not reach the hub: {error}"));
    app.next_retry()
}

#[test]
fn retry_after_doubles_from_half_a_second_and_caps_at_thirty() {
    let mut app = App::new(PathBuf::from("/w"));
    let delays: Vec<Option<Duration>> = (1..=8).map(|_| app.next_retry()).collect();
    let want: Vec<Option<Duration>> = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
        .into_iter()
        .map(|ms| Some(Duration::from_millis(ms)))
        .collect();
    assert_eq!(delays, want);
}

#[test]
fn failures_saturate_at_the_maximum() {
    let mut app = App::new(PathBuf::from("/w"));
    app.reconnect.failures = u32::MAX;
    assert_eq!(app.next_retry(), Some(Duration::from_secs(30)));
    assert_eq!(app.reconnect.failures, u32::MAX);
    // Never up: the attempt number saturates too.
    app.link = Link::Down;
    assert_eq!(app.banner(), banner(u64::from(u32::MAX)));
}

#[test]
fn a_hub_hello_resets_the_count() {
    let mut app = App::new(PathBuf::from("/w"));
    for _ in 0..3 {
        failed_connect(&mut app, "refused");
    }
    app.on_line(hello());
    assert_eq!(app.reconnect.failures, 0);
    app.disconnected();
    assert_eq!(app.next_retry(), Some(Duration::from_millis(500)));
}

#[test]
fn a_refused_schema_never_retries() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(newer());
    assert_eq!(app.link, Link::Refused);
    assert_eq!(app.next_retry(), None);
    assert_eq!(app.reconnect.failures, 0);
    // The reader's end of the refused stream changes nothing.
    app.disconnected();
    assert_eq!(app.next_retry(), None);
    assert_eq!(app.banner(), None);
}

#[test]
fn a_refused_schema_after_failed_attempts_still_says_why() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "refused");
    failed_connect(&mut app, "refused again");
    assert_eq!(app.notice(), Some("Could not reach the hub: refused"));
    app.on_line(newer());
    assert_eq!(
        app.notice(),
        Some(
            format!(
                "The hub runs schema version {}; this terminal reads {}.",
                contract::SCHEMA_VERSION + 1,
                contract::SCHEMA_VERSION
            )
            .as_str()
        )
    );
}

#[test]
fn the_banner_after_a_drop_names_attempt_one_then_two() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(hello());
    app.disconnected();
    app.next_retry();
    assert_eq!(app.banner(), banner(1));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(2));
}

#[test]
fn before_the_first_connection_the_banner_starts_at_attempt_two() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(2));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(3));
}

#[test]
fn no_banner_while_waiting_up_or_refused() {
    let mut waiting = App::new(PathBuf::from("/w"));
    waiting.reconnect.failures = 1;
    assert_eq!(waiting.link, Link::Waiting);
    assert_eq!(waiting.banner(), None);
    let mut up = App::new(PathBuf::from("/w"));
    up.on_line(hello());
    up.reconnect.failures = 1;
    assert_eq!(up.banner(), None);
    let mut refused = App::new(PathBuf::from("/w"));
    refused.on_line(newer());
    refused.reconnect.failures = 1;
    assert_eq!(refused.banner(), None);
    // The same count while down shows it.
    let mut down = App::new(PathBuf::from("/w"));
    down.on_line(hello());
    down.disconnected();
    down.reconnect.failures = 1;
    assert_eq!(down.banner(), banner(1));
}

#[test]
fn no_banner_at_zero_failures_while_down() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(hello());
    // A failed write: the link is down before the reader's end arrives.
    app.write_failed(&[]);
    assert_eq!(app.link, Link::Down);
    assert_eq!(app.banner(), None);
    app.next_retry();
    assert_eq!(app.banner(), banner(1));
}

#[test]
fn only_the_first_failure_is_a_notice() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "first");
    assert_eq!(app.notice(), Some("Could not reach the hub: first"));
    failed_connect(&mut app, "second");
    assert_eq!(app.notice(), Some("Could not reach the hub: first"));
}

#[test]
fn enter_while_refused_keeps_the_draft_and_holds_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(newer());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "hi");
    assert!(app.held.is_empty());
    assert!(app.pending.is_empty());
}

/// The banner row's y and the conversation's top row, drawn at the app's
/// size.
fn drawn(app: &App) -> (usize, usize) {
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let row = text
        .lines()
        .position(|line| line.contains("reconnecting (attempt 1)"))
        .unwrap_or_else(|| panic!("the banner is drawn:\n{text}"));
    let layout = app
        .chrome()
        .layout()
        .unwrap_or_else(|| panic!("the session screen's layout"));
    (row, usize::from(crate::view::chrome::body(&layout).y))
}

#[test]
fn the_banner_row_is_counted_in_conversation_height() {
    for (width, height) in [(160, 40), (100, 30)] {
        let mut app = home();
        app.on_line(hello());
        app.attach(contract::SessionId(S_A.to_owned()));
        app.set_size(width, height);
        let before = app.conversation_height();
        app.disconnected();
        app.next_retry();
        assert_eq!(app.conversation_height(), before - 1, "{width}x{height}");
        // The conversation ends on the row above the banner.
        let (banner, top) = drawn(&app);
        assert_eq!(app.conversation_height(), banner - top, "{width}x{height}");
    }
}
