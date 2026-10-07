//! The repository offer's frame: snapshots, its click targets and the
//! cursor.

use contract::clock::Clock;
use contract::events::OfferDecision;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use std::path::PathBuf;

use super::super::{cursor, render, text};
use crate::app::{App, Effect};
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::offer::Spot;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";

/// A session line of `kind` from [`S_A`].
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

/// An app connected, attached to [`S_A`], `width` by `height`, holding an
/// offer of `items`.
fn offered(items: Value, width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(width, height);
    app.on_line(session_line(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": items}),
    ));
    assert!(app.offer_open());
    app
}

/// An MCP server item named `name`.
fn server(name: &str, required: bool) -> Value {
    json!({"kind": "mcp_server", "name": name, "hash": "h", "required": required,
        "summary": format!("MCP server: {name}\ndeclared in: .fiber/config.json\nruns: /bin/echo")})
}

/// Draws `app` at `width` by `height`, returning the screen and the
/// targets.
fn draw(app: &App, width: u16, height: u16) -> (String, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    (text(&buf), targets)
}

/// The offer's targets drawn.
fn offer_targets(targets: &[Target]) -> Vec<Target> {
    targets
        .iter()
        .filter(|target| matches!(target.id, TargetId::Offer(_)))
        .copied()
        .collect()
}

/// The rect of the target `spot`.
fn rect_of(targets: &[Target], spot: Spot) -> Option<Rect> {
    targets
        .iter()
        .find(|target| target.id == TargetId::Offer(spot))
        .map(|target| target.rect)
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

#[test]
fn one_mcp_server() {
    let app = offered(json!([server("db", true)]), 80, 24);
    insta::assert_snapshot!("offer_one_mcp_server", draw(&app, 80, 24).0);
}

#[test]
fn an_extension_with_its_version_and_diff() {
    let item = json!({"kind": "extension", "name": "lint", "hash": "h", "required": true,
        "version": "1.2.0", "summary": "extension lint 1.2.0\nfiles: index.ts",
        "diff": "--- a/index.ts\n+++ b/index.ts\n@@ -1 +1 @@\n-old()\n+new()"});
    let app = offered(json!([item]), 80, 24);
    insta::assert_snapshot!("offer_extension_with_diff", draw(&app, 80, 24).0);
}

#[test]
fn three_items_scrolled_to_send() {
    let mut app = offered(
        json!([server("a", false), server("b", false), server("c", false)]),
        80,
        16,
    );
    for _ in 0..3 {
        press(&mut app, Key::Down);
    }
    let (screen, targets) = draw(&app, 80, 16);
    assert!(rect_of(&targets, Spot::Send).is_some());
    insta::assert_snapshot!("offer_three_items_on_send", screen);
}

#[test]
fn the_chosen_chip_after_right_twice() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    app.on_edit(Edit::Right);
    app.on_edit(Edit::Right);
    insta::assert_snapshot!("offer_never_chosen", draw(&app, 80, 24).0);
}

#[test]
fn a_long_summary_wraps_at_40_columns() {
    let item = json!({"kind": "hook", "name": "fmt", "hash": "h", "required": false,
        "summary": "runs: cargo fmt --all -- --check --config-path rustfmt.toml"});
    let app = offered(json!([item]), 40, 16);
    let (screen, targets) = draw(&app, 40, 16);
    insta::assert_snapshot!("offer_wrapped_40", screen);
    for target in offer_targets(&targets) {
        assert!(target.rect.right() <= 40, "{target:?}");
    }
    let never = Spot::Choice {
        item: 0,
        decision: OfferDecision::Never,
    };
    assert!(rect_of(&targets, never).is_some());
}

#[test]
fn a_narrow_view_drops_a_chip_past_its_width() {
    let app = offered(json!([server("db", false)]), 16, 16);
    let (_, targets) = draw(&app, 16, 16);
    let never = Spot::Choice {
        item: 0,
        decision: OfferDecision::Never,
    };
    assert!(rect_of(&targets, never).is_none());
    // "[skip]" starts at 14 and is cut at the edge, below the wrapped
    // header rows.
    let skip = Spot::Choice {
        item: 0,
        decision: OfferDecision::Skip,
    };
    assert_eq!(rect_of(&targets, skip), Some(Rect::new(14, 7, 2, 1)));
}

#[test]
fn a_crlf_diff_draws_without_carriage_returns() {
    let item = json!({"kind": "extension", "name": "lint", "hash": "h", "required": false,
        "version": "1.0.0", "summary": "extension lint\r\nfiles: a.ts",
        "diff": "-old\r\n+new\r\n"});
    let app = offered(json!([item]), 80, 24);
    let (screen, _) = draw(&app, 80, 24);
    assert!(!screen.contains('\r'));
    insta::assert_snapshot!("offer_crlf_diff", screen);
}

#[test]
fn the_notice_list_hides_the_offers_targets() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    app.on_line(session_line(
        "notice",
        json!({"code": "internal", "message": "hello"}),
    ));
    assert!(!offer_targets(&draw(&app, 80, 24).1).is_empty());
    app.open_more_notices();
    let (screen, targets) = draw(&app, 80, 24);
    assert!(offer_targets(&targets).is_empty());
    assert!(screen.contains("hello"));
}

#[test]
fn no_cursor_while_the_offer_is_open() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    let area = Rect::new(0, 0, 80, 24);
    assert!(cursor(&app, area).is_none());
    press(&mut app, Key::Esc);
    assert!(cursor(&app, area).is_some());
}

#[test]
fn clicking_a_chip_sets_its_decision_and_moves_the_cursor() {
    let mut app = offered(json!([server("a", false), server("b", false)]), 80, 30);
    let (_, targets) = draw(&app, 80, 30);
    let spot = Spot::Choice {
        item: 1,
        decision: OfferDecision::Approve,
    };
    let Some(rect) = rect_of(&targets, spot) else {
        panic!("the chip is drawn");
    };
    assert_eq!(rect.width, 9);
    assert_eq!(app.on_click(TargetId::Offer(spot)), Effect::None);
    let (screen, _) = draw(&app, 80, 30);
    assert!(screen.contains("› MCP server b"), "{screen}");
    assert!(screen.contains("    [approve]  skip   never"), "{screen}");
}

#[test]
fn the_send_target_sends_and_the_cross_puts_it_aside() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    let (_, targets) = draw(&app, 80, 24);
    assert_eq!(rect_of(&targets, Spot::Close), Some(Rect::new(79, 0, 1, 1)));
    assert!(rect_of(&targets, Spot::Send).is_some());
    let Effect::Send(lines) = app.on_click(TargetId::Offer(Spot::Send)) else {
        panic!("Send sends");
    };
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_default();
    assert_eq!(line["args"]["decisions"], json!(["skip"]));
    let mut app = offered(json!([server("db", false)]), 80, 24);
    app.on_click(TargetId::Offer(Spot::Close));
    assert!(!app.offer_open());
    let (screen, _) = draw(&app, 80, 24);
    assert!(screen.contains("! 1 waiting"), "{screen}");
}

#[test]
fn a_taller_screen_shows_the_whole_offer_after_scrolling() {
    let mut app = offered(
        json!([server("a", false), server("b", false), server("c", false)]),
        80,
        8,
    );
    for _ in 0..5 {
        press(&mut app, Key::PageDown);
    }
    assert!(!draw(&app, 80, 8).0.starts_with("Repository code"));
    // The scroll is past the end of a 40-row view, which shows it all.
    app.set_size(80, 40);
    let (screen, _) = draw(&app, 80, 40);
    assert!(screen.starts_with("Repository code"), "{screen}");
    assert!(screen.contains("Send"), "{screen}");
}
