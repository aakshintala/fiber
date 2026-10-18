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

fn source_app(width: u16, height: u16, answer: Option<Result<&str, &str>>) -> App {
    let mut app = offered(
        json!([{"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "hook", "files": ["tools/db.sh", "tools/b.sh"]}]),
        width,
        height,
    );
    let Effect::Send(lines) = press(&mut app, Key::Enter) else {
        panic!("Enter asks for source");
    };
    let command: Value = serde_json::from_str(&lines[0]).unwrap();
    if let Some(answer) = answer {
        let (kind, payload) = match answer {
            Ok(text) => (
                "command_accepted",
                json!({"command_id": command["id"], "result": {"text": text}}),
            ),
            Err(message) => (
                "command_rejected",
                json!({"command_id": command["id"], "code": "invalid_arguments", "message": message}),
            ),
        };
        app.on_line(session_line(kind, payload));
    }
    app
}

#[test]
fn source_text() {
    let app = source_app(80, 24, Some(Ok("#!/bin/sh\n\techo marker\nexit 0\n")));
    let (screen, targets) = draw(&app, 80, 24);
    insta::assert_snapshot!("source_text", screen);
    assert_eq!(cursor(&app, Rect::new(0, 0, 80, 24)), None);
    let targets = offer_targets(&targets);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, TargetId::Offer(Spot::Close));
}

#[test]
fn source_loading() {
    insta::assert_snapshot!("source_loading", draw(&source_app(80, 24, None), 80, 24).0);
}

#[test]
fn source_refused() {
    insta::assert_snapshot!(
        "source_refused",
        draw(
            &source_app(80, 24, Some(Err("That path is not in the pending offer."))),
            80,
            24
        )
        .0
    );
}

#[test]
fn source_scrolled() {
    let text = (0..60).map(|n| format!("line {n}\n")).collect::<String>();
    let mut app = source_app(80, 24, Some(Ok(&text)));
    press(&mut app, Key::PageDown);
    insta::assert_snapshot!("source_scrolled", draw(&app, 80, 24).0);
}

#[test]
fn source_wrapped_40() {
    let app = source_app(
        40,
        24,
        Some(Ok(
            "    echo a long line that keeps its indentation and wraps across the narrow source view without dropping any spaces",
        )),
    );
    insta::assert_snapshot!("source_wrapped_40", draw(&app, 40, 24).0);
}

#[test]
fn a_line_past_65535_rows_scrolls_to_its_end() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let text = format!("{}FINALCHUNK", "0123456789".repeat(70_000));
        assert_eq!(text.len(), 700_010);
        let mut app = source_app(10, 100, Some(Ok(&text)));
        // Shrinking page sizes reach the far end without rebuilding 70,000
        // rows once for each tiny page. The final drawing is still 10 by 5.
        for _ in 0..128 {
            let (rows, top) = app.offer_rows(10).unwrap();
            let total: usize = rows.iter().map(|row| row.height(10)).sum();
            if top >= total.saturating_sub(5) {
                let area = Rect::new(0, 0, 10, 5);
                let mut buf = Buffer::empty(area);
                let mut targets = Vec::new();
                super::render(&rows, top, area, &mut buf, &mut targets);
                tx.send(super::super::text(&buf)).unwrap();
                return;
            }
            let remaining = total - top;
            let height = if remaining < 50 {
                1
            } else {
                u16::try_from(remaining / 2 + 20).unwrap()
            };
            app.set_size(10, height);
            press(&mut app, Key::PageDown);
        }
        panic!("PageDown reaches the end within 128 shrinking pages");
    });
    let screen = fakes::Deadline::after(std::time::Duration::from_secs(30))
        .recv_or_fail(&rx, "scroll beyond 65,535 source rows");
    assert!(screen.contains("FINALCHUNK"), "{screen}");
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
    // The 53-column choices row is cut at the width: approve and skip
    // draw, never starts past it.
    for decision in [OfferDecision::Approve, OfferDecision::Skip] {
        assert!(
            rect_of(&targets, Spot::Choice { item: 0, decision }).is_some(),
            "{decision:?} draws"
        );
    }
    let never = Spot::Choice {
        item: 0,
        decision: OfferDecision::Never,
    };
    assert!(rect_of(&targets, never).is_none());
}

#[test]
fn a_narrow_view_drops_a_chip_past_its_width() {
    let app = offered(json!([server("db", false)]), 16, 16);
    let (_, targets) = draw(&app, 16, 16);
    // Approve covers 2..13 and draws whole; skip starts at 16, past
    // the width, and never starts later still.
    let approve = Spot::Choice {
        item: 0,
        decision: OfferDecision::Approve,
    };
    let approve_rect = rect_of(&targets, approve);
    assert!(approve_rect.is_some_and(|rect| rect.width == 11));
    for decision in [OfferDecision::Skip, OfferDecision::Never] {
        assert!(
            rect_of(&targets, Spot::Choice { item: 0, decision }).is_none(),
            "{decision:?} is past the width"
        );
    }
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

/// A free-text form with one question lands on its words row, so its
/// caret shows at once.
fn ask_free_text(app: &mut App) {
    app.on_line(session_line(
        "interaction_requested",
        json!({"request_id": "r_4f", "kind": "form", "action_ids": ["a_1"],
            "fields": [{"header": "Name", "question": "What name?"}]}),
    ));
}

#[test]
fn a_shown_forms_caret_wins_over_the_open_offer() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    ask_free_text(&mut app);
    assert!(app.offer_open());
    assert!(app.panel().is_some());
    // The form's caret shows even while the offer is open.
    let area = Rect::new(0, 0, 80, 24);
    let caret = cursor(&app, area);
    assert!(caret.is_some());
    // The same form with no offer draws the cursor in the same cell.
    let mut alone = offered(json!([server("db", false)]), 80, 24);
    press(&mut alone, Key::Esc);
    assert!(!alone.offer_open());
    ask_free_text(&mut alone);
    assert_eq!(cursor(&alone, area), caret);
}

#[test]
fn an_approval_without_a_caret_keeps_the_offer_hiding_the_cursor() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    app.on_line(session_line(
        "permission_requested",
        json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": "ls"}}),
    ));
    assert!(app.offer_open());
    assert!(app.panel().is_some());
    // No words row has the cursor, so the open offer still hides it.
    let area = Rect::new(0, 0, 80, 24);
    assert_eq!(cursor(&app, area), None);
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
    assert_eq!(rect.width, 11);
    assert_eq!(app.on_click(TargetId::Offer(spot)), Effect::None);
    let (screen, _) = draw(&app, 80, 30);
    assert!(screen.contains("▸ MCP server b"), "{screen}");
    assert!(
        screen.contains("  (•) approve   ( ) skip for this session   ( ) never"),
        "{screen}"
    );
}

#[test]
fn the_send_target_sends_and_the_cross_puts_it_aside() {
    let mut app = offered(json!([server("db", false)]), 80, 24);
    let (_, targets) = draw(&app, 80, 24);
    assert_eq!(rect_of(&targets, Spot::Close), Some(Rect::new(0, 0, 80, 1)));
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
    assert!(
        screen.contains("  ⚠ 1 approvals waiting · click to reopen"),
        "{screen}"
    );
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
    assert!(!draw(&app, 80, 8).0.starts_with(" Offer"));
    // The scroll is past the end of a 40-row view, which shows it all.
    app.set_size(80, 40);
    let (screen, _) = draw(&app, 80, 40);
    assert!(screen.starts_with(" Offer"), "{screen}");
    assert!(screen.contains("Send"), "{screen}");
}

#[test]
fn offer_diff_uses_the_changed_files_hunk_look() {
    use crate::theme::Role;
    use ratatui::style::Modifier;
    let item = json!({"kind": "extension", "name": "lint", "hash": "h", "required": true,
        "version": "1.2.0", "summary": "extension lint 1.2.0\nfiles: index.ts",
        "diff": "--- a/index.ts\n+++ b/index.ts\n@@ -1 +1 @@\n-old()\n+new()"});
    let app = offered(json!([item]), 80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let screen = text(&buf);
    assert!(!screen.contains("--- a/"), "{screen}");
    let row_text = |y: u16| {
        (0..80)
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect::<String>()
    };
    let hunk = (0..24)
        .find(|y| row_text(*y).contains("@@ -1 +1 @@"))
        .expect("hunk row");
    let at = row_text(hunk).find("@@ -1 +1 @@").expect("hunk text");
    assert!(
        (at..at + "@@ -1 +1 @@".len()).all(|x| {
            let x = u16::try_from(x).unwrap_or(u16::MAX);
            buf[(x, hunk)].modifier.contains(Modifier::DIM)
        }),
        "hunk row dim"
    );
    assert_eq!(buf[(4, hunk)].fg, Role::Muted.color());
    // The pair's differing words draw in reverse video under their colour.
    let added = (0..24)
        .find(|y| row_text(*y).contains("+new()"))
        .expect("added row");
    assert!(
        (0..80).any(|x| buf[(x, added)].modifier.contains(Modifier::REVERSED)
            && buf[(x, added)].fg == Role::Added.color()),
        "marked added words"
    );
    let removed = (0..24)
        .find(|y| row_text(*y).contains("-old()"))
        .expect("removed row");
    assert!(
        (0..80).any(|x| buf[(x, removed)].modifier.contains(Modifier::REVERSED)
            && buf[(x, removed)].fg == Role::Removed.color()),
        "marked removed words"
    );
}
