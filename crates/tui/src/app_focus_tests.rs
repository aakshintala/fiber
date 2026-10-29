//! Tests for navigate mode at the app level: focus, order, stepping, Tab,
//! Esc and Enter.

use super::super::{App, Effect, Target};
use crate::app::rail::Spot as RailSpot;
use crate::app::testkit::{SENDER, SESSION, attached, now, sender_status};
use crate::focus::Regions;
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::{Target as Hit, TargetId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

fn envelope(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
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

/// An app with one turn of `count` read groups and their replies, closed:
/// one line target per group.
fn groups(count: usize, width: u16, height: u16) -> App {
    let mut app = attached(width, height);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    for i in 1..=count {
        app.on_line(envelope(
            "tool_call_requested",
            serde_json::json!({"name": "read", "arguments": {"path": format!("src/{i}.rs")}}),
            Some(format!("a_{i}").as_str()),
        ));
        app.on_line(envelope(
            "tool_call_completed",
            serde_json::json!({"status": "completed",
                "content": [{"type": "text", "text": "ok"}]}),
            Some(format!("a_{i}").as_str()),
        ));
        app.on_line(envelope(
            "assistant_message_delta",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
        app.on_line(envelope(
            "text_completed",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
    }
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    assert_eq!(
        app.targets().len(),
        count,
        "the fixture folds to one target per group"
    );
    app
}

/// The steering queue with `rows` of (text, command id).
fn steering_queue(rows: &[(&str, Option<&str>)]) -> Line {
    let messages: Vec<serde_json::Value> = rows
        .iter()
        .map(|(text, id)| {
            let mut message = serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "source": "driver",
            });
            if let (Some(id), Some(object)) = (id, message.as_object_mut()) {
                object.insert("command_id".to_owned(), serde_json::json!(id));
            }
            message
        })
        .collect();
    envelope(
        "steering_queue",
        serde_json::json!({"messages": messages}),
        None,
    )
}

/// A review request offering a rule, as the hub relays it.
fn permission_requested() -> Line {
    envelope(
        "permission_requested",
        serde_json::json!({
            "request_id": "r_1", "effects": ["executes"], "reversible": true,
            "step": "review",
            "rule": {"subject": "npm test --watch", "prefix": "npm test"},
        }),
        Some("a_r1"),
    )
}

fn hub_hello() -> contract::HubLine {
    contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }
}

/// Renders `app` at its size, takes the frame's targets, and returns them.
fn frame(app: &mut App) -> Vec<Hit> {
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
    targets
}

/// `key` through `on_key`, then the frame it draws.
fn key(app: &mut App, key: Key) -> Effect {
    let effect = app.on_key(key, now());
    frame(app);
    effect
}

/// The screen as text at the app's size.
fn screen(app: &App) -> String {
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// The drawn rect of the focused stop `id`.
fn rect_of(targets: &[Hit], id: TargetId) -> Rect {
    targets
        .iter()
        .find(|target| target.id == id)
        .map(|target| target.rect)
        .unwrap_or_else(|| panic!("{id:?} is not drawn"))
}

/// The regions and targets of a screen with a panel and a rail: no panel
/// or rail ids exist until #669, so a notice and "+N more" stand in for
/// the panel's cards, and handoff notes for the rail's.
fn screened() -> (Regions, Vec<Hit>) {
    let regions = Regions {
        panel: Some(Rect::new(60, 0, 20, 20)),
        rail: Some(Rect::new(0, 0, 12, 20)),
    };
    let targets = vec![
        Hit {
            id: TargetId::Line(Target::Note(1)),
            rect: Rect::new(0, 4, 12, 2),
        },
        Hit {
            id: TargetId::Line(Target::Call(1)),
            rect: Rect::new(12, 8, 48, 1),
        },
        Hit {
            id: TargetId::Notice(5),
            rect: Rect::new(60, 2, 20, 3),
        },
        Hit {
            id: TargetId::Line(Target::Group(0)),
            rect: Rect::new(12, 3, 48, 1),
        },
        Hit {
            id: TargetId::MoreNotices,
            rect: Rect::new(60, 6, 20, 4),
        },
        Hit {
            id: TargetId::Rail(RailSpot::Card(7)),
            rect: Rect::new(0, 1, 12, 2),
        },
    ];
    (regions, targets)
}

fn group_ids(app: &App) -> Vec<TargetId> {
    app.targets()
        .into_iter()
        .map(|(_, target)| TargetId::Line(target))
        .collect()
}

#[test]
fn shift_tab_focuses_the_newest_item_and_hides_the_cursor() {
    let mut app = groups(3, 60, 24);
    let area = Rect::new(0, 0, 60, 24);
    assert_eq!(app.focused(), None);
    assert!(crate::view::cursor(&app, area).is_some());
    assert_eq!(key(&mut app, Key::BackTab), Effect::None);
    let stops = frame(&mut app);
    let newest = group_ids(&app).last().copied().expect("a group");
    assert_eq!(app.focused(), Some(newest));
    assert!(stops.iter().any(|target| target.id == newest));
    assert_eq!(app.top(), None);
    assert_eq!(crate::view::cursor(&app, area), None);
}

#[test]
fn shift_tab_with_no_stops_keeps_the_input_box() {
    let mut app = attached(60, 24);
    frame(&mut app);
    assert_eq!(key(&mut app, Key::BackTab), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn shift_tab_with_no_items_focuses_the_last_stop() {
    let mut app = attached(60, 24);
    let targets = vec![
        Hit {
            id: TargetId::Steering(0),
            rect: Rect::new(0, 4, 10, 1),
        },
        Hit {
            id: TargetId::Badge,
            rect: Rect::new(0, 5, 10, 1),
        },
    ];
    app.drawn(&targets);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Badge));
}

#[test]
fn j_and_down_step_on_k_and_up_step_back_and_the_ends_hold() {
    let mut app = groups(3, 60, 24);
    let ids = group_ids(&app);
    assert_eq!(ids.len(), 3);
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), ids.last().copied());
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), ids.get(1).copied());
    key(&mut app, Key::Char('k'));
    assert_eq!(app.focused(), ids.first().copied());
    // Each turn is a stop before its own group lines: up from the
    // first group reaches its turn, and the turn holds at the top.
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    key(&mut app, Key::Down);
    assert_eq!(app.focused(), ids.first().copied());
    key(&mut app, Key::Down);
    assert_eq!(app.focused(), ids.get(1).copied());
    key(&mut app, Key::Char('j'));
    assert_eq!(app.focused(), ids.last().copied());
    key(&mut app, Key::Down);
    assert_eq!(app.focused(), ids.last().copied());
}

#[test]
fn stepping_up_past_the_top_scrolls_the_item_above_onto_the_top_row() {
    let mut app = groups(12, 60, 8);
    let ids = group_ids(&app);
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), ids.last().copied());
    for (n, want) in ids.iter().rev().skip(1).enumerate() {
        key(&mut app, Key::Up);
        assert_eq!(app.focused(), Some(*want), "step {n}");
        let stops = frame(&mut app);
        assert!(
            stops.iter().any(|target| target.id == *want),
            "step {n}: {want:?} is drawn"
        );
    }
    let stops = frame(&mut app);
    let first = ids.first().copied().expect("a group");
    assert_eq!(app.focused(), Some(first));
    assert_eq!(rect_of(&stops, first).y, 0);
    assert!(app.top().is_some());
    // One more Up reaches the turn, with its first row on row 0.
    key(&mut app, Key::Up);
    let stops = frame(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    assert_eq!(rect_of(&stops, TargetId::Turn(0)).y, 0);
}

#[test]
fn stepping_down_past_the_bottom_scrolls_the_item_below_onto_the_last_row() {
    let mut app = groups(12, 60, 8);
    let ids = group_ids(&app);
    key(&mut app, Key::BackTab);
    for want in ids.iter().rev().skip(1) {
        key(&mut app, Key::Up);
        assert_eq!(app.focused(), Some(*want));
    }
    for want in ids.iter().skip(1) {
        let before = app.top();
        key(&mut app, Key::Down);
        assert_eq!(app.focused(), Some(*want));
        let stops = frame(&mut app);
        assert!(
            stops.iter().any(|target| target.id == *want),
            "{want:?} is drawn"
        );
        if app.top() != before {
            let rect = rect_of(&stops, *want);
            assert_eq!(
                usize::from(rect.y) + usize::from(rect.height),
                app.conversation_height(),
                "{want:?} sits on the last conversation row"
            );
        }
    }
}

#[test]
fn an_item_off_screen_below_wins_over_the_stop_under_the_conversation() {
    let mut app = groups(12, 60, 10);
    app.on_line(steering_queue(&[("steer me", Some("c_1"))]));
    frame(&mut app);
    key(&mut app, Key::BackTab);
    let items = app.items();
    let first = items.first().copied().expect("an item");
    // Up to the first item, scrolling the conversation; every item
    // below stays drawn on the way up, so walking up never leaves the
    // item below off screen.
    for _ in 0..40 {
        if app.focused() == Some(first.1) {
            break;
        }
        key(&mut app, Key::Up);
    }
    assert_eq!(app.focused(), Some(first.1));
    assert!(app.top().is_some());
    // Down again: the first item below the lowest drawn line is off
    // screen, and Down must reach it rather than the steering stop
    // drawn under the conversation.
    for _ in 0..40 {
        let stops = frame(&mut app);
        let focused = app.focused().expect("focus");
        let at = items
            .iter()
            .position(|(_, item)| *item == focused)
            .expect("the focused stop is an item");
        let after = items.get(at.saturating_add(1));
        let after_drawn =
            after.is_some_and(|(_, next)| stops.iter().any(|target| target.id == *next));
        let lowest = stops
            .iter()
            .filter(|target| matches!(target.id, TargetId::Line(_)))
            .all(|target| target.rect.y <= rect_of(&stops, focused).y);
        if after.is_some() && !after_drawn && lowest {
            let (_, next) = after.copied().expect("the item below");
            key(&mut app, Key::Down);
            assert_eq!(app.focused(), Some(next));
            assert!(matches!(next, TargetId::Line(_)));
            return;
        }
        key(&mut app, Key::Down);
    }
    panic!("never held an off-screen item below the lowest drawn line");
}

#[test]
fn a_notice_above_the_working_line_is_a_stop_before_its_cross() {
    let mut app = groups(12, 60, 10);
    while app.top() != Some(0) {
        key(&mut app, Key::PageUp);
    }
    app.notices.push("extension_failed", "Saved.".to_owned());
    frame(&mut app);
    key(&mut app, Key::BackTab);
    // Up to the turn, whose stop is clipped to row 0 and drawn before
    // the line on row 0.
    for _ in 0..60 {
        if app.focused() == Some(TargetId::Turn(0)) {
            break;
        }
        key(&mut app, Key::Up);
    }
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    // Down from the turn walks every stop in conversation order; the
    // notice rows sit above the working line, so the notice and its
    // cross come together at the walk's bottom end.
    let mut found = false;
    for _ in 0..200 {
        key(&mut app, Key::Down);
        if app.focused() == Some(TargetId::Notice(0)) {
            found = true;
            break;
        }
    }
    assert!(found, "the notice is a stop");
    key(&mut app, Key::Down);
    assert_eq!(app.focused(), Some(TargetId::DismissNotice(0)));
    let stops = frame(&mut app);
    assert!(
        stops
            .iter()
            .any(|target| target.id == TargetId::DismissNotice(0))
    );
}

#[test]
fn enter_opens_the_focused_group_as_a_click_does() {
    let mut app = groups(1, 60, 24);
    let group = group_ids(&app).first().copied().expect("a group");
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), Some(group));
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(screen(&app).contains("src/1.rs"), "the ledger opened");
    assert_eq!(app.focused(), Some(group));
}

#[test]
fn enter_on_a_steering_row_puts_it_in_the_draft_and_focus_back_in_the_input_box() {
    let mut app = attached(60, 24);
    app.on_line(steering_queue(&[("fix it", Some("c_1"))]));
    frame(&mut app);
    key(&mut app, Key::BackTab);
    for _ in 0..4 {
        if app.focused() == Some(TargetId::Steering(0)) {
            break;
        }
        key(&mut app, Key::Up);
    }
    assert_eq!(app.focused(), Some(TargetId::Steering(0)));
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(app.input().expand(), "fix it");
    assert_eq!(app.focused(), None);
}

#[test]
fn enter_on_the_badge_opens_the_queue_and_the_next_frame_returns_focus() {
    let mut app = attached(60, 12);
    app.on_line(permission_requested());
    app.put_aside();
    let stops = frame(&mut app);
    assert!(stops.iter().any(|target| target.id == TargetId::Badge));
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), Some(TargetId::Badge));
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert!(app.panel().is_some());
    assert_eq!(app.focused(), None);
}

#[test]
fn enter_on_new_messages_below_follows_and_focus_returns() {
    let mut app = attached(60, 12);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    for i in 1..=20 {
        app.on_line(envelope(
            "assistant_message_delta",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
        app.on_line(envelope(
            "text_completed",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
    }
    // The turn stays open: new output below still folds into it.
    assert!(app.targets().is_empty(), "replies open nothing");
    key(&mut app, Key::PageUp);
    assert!(app.top().is_some());
    app.on_line(envelope(
        "assistant_message_delta",
        serde_json::json!({"text": "fresh"}),
        Some("m_99"),
    ));
    let stops = frame(&mut app);
    assert!(stops.iter().any(|target| target.id == TargetId::NewBelow));
    // The replies-only turn is itself an item; below it in focus order
    // comes "New messages below".
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    key(&mut app, Key::Down);
    assert_eq!(app.focused(), Some(TargetId::NewBelow));
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(app.top(), None);
    assert_eq!(app.focused(), None);
}

#[test]
fn esc_returns_focus_without_interrupting_then_esc_interrupts() {
    let mut app = attached(60, 24);
    app.on_line(Line::Hub(hub_hello()));
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/1.rs"}}),
        Some("a_1"),
    ));
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_1"),
    ));
    frame(&mut app);
    key(&mut app, Key::BackTab);
    assert!(app.focused().is_some());
    assert_eq!(key(&mut app, Key::Esc), Effect::None);
    assert_eq!(app.focused(), None);
    let Effect::Send(lines) = key(&mut app, Key::Esc) else {
        panic!("the second Esc interrupts");
    };
    assert_eq!(lines.len(), 1);
    assert!(lines.first().is_some_and(|line| line.contains("cancel")));
}

#[test]
fn esc_closes_the_notice_overlay_before_leaving_navigate_mode() {
    let mut app = attached(60, 24);
    app.notices.push("extension_failed", "Saved.".to_owned());
    app.open_notice(0);
    let badge = Hit {
        id: TargetId::Badge,
        rect: Rect::new(0, 5, 10, 1),
    };
    app.drawn(&[badge]);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    app.drawn(&[badge]);
    assert_eq!(app.focused(), Some(TargetId::Badge));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.drawn(&[badge]);
    assert!(app.notice_overlay().is_none());
    assert_eq!(app.focused(), Some(TargetId::Badge));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.drawn(&[badge]);
    assert_eq!(app.focused(), None);
}

#[test]
fn keys_the_input_box_owns_do_nothing_while_navigating() {
    let mut app = groups(1, 60, 24);
    key(&mut app, Key::Char('x'));
    assert_eq!(app.input().expand(), "x");
    key(&mut app, Key::BackTab);
    let group = group_ids(&app).first().copied().expect("a group");
    assert_eq!(app.focused(), Some(group));
    assert_eq!(key(&mut app, Key::Char('a')), Effect::None);
    assert_eq!(key(&mut app, Key::Backspace), Effect::None);
    assert_eq!(key(&mut app, Key::CtrlR), Effect::None);
    assert_eq!(app.on_edit(Edit::Paste("p".to_owned())), Effect::None);
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(app.input().expand(), "x");
    assert!(app.completions().is_none());
    assert_eq!(app.focused(), Some(group));
}

#[test]
fn global_keys_still_work_while_navigating() {
    let mut app = groups(3, 60, 12);
    frame(&mut app);
    app.on_key(Key::BackTab, now());
    frame(&mut app);
    let group = app.focused().expect("focus");
    // No frames between: the keys act on the app, and focus never moves.
    app.on_key(Key::F1, now());
    assert_eq!(app.keymap_top(), Some(0));
    app.on_key(Key::Esc, now());
    assert_eq!(app.keymap_top(), None);
    assert_eq!(app.focused(), Some(group));
    app.on_key(Key::CtrlO, now());
    assert!(
        screen(&app).contains("src/"),
        "the ledgers opened: {}",
        screen(&app)
    );
    app.on_key(Key::PageUp, now());
    assert!(app.top().is_some());
    app.on_key(Key::End, now());
    assert_eq!(app.top(), None);
    assert_eq!(app.focused(), Some(group));
    frame(&mut app);
    assert_eq!(app.focused(), Some(group));
}

#[test]
fn tab_moves_to_the_panel_then_the_rail() {
    let mut app = attached(60, 20);
    let (regions, targets) = screened();
    app.set_regions(regions);
    app.drawn(&targets);
    app.on_key(Key::BackTab, now());
    app.drawn(&targets);
    assert_eq!(
        app.focused(),
        Some(TargetId::Line(Target::Call(1))),
        "with no items, the last conversation stop"
    );
    app.on_key(Key::Tab, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Notice(5)));
    app.on_key(Key::Tab, now());
    app.drawn(&targets);
    // Tab onto the rail focuses the card, not an App target; with no
    // home the settle drops rail focus, so the next Tab rests.
    assert_eq!(app.focused(), None);
    app.on_key(Key::Tab, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), None);
}

#[test]
fn tab_skips_a_hidden_panel() {
    let mut app = attached(60, 20);
    let (regions, targets) = screened();
    let regions = Regions {
        panel: None,
        rail: regions.rail,
    };
    let targets: Vec<Hit> = targets
        .into_iter()
        .filter(|target| !matches!(target.id, TargetId::Notice(_) | TargetId::MoreNotices))
        .collect();
    app.set_regions(regions);
    app.drawn(&targets);
    app.on_key(Key::BackTab, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Line(Target::Call(1))));
    app.on_key(Key::Tab, now());
    app.drawn(&targets);
    // Tab onto the rail focuses the card, not an App target; with no
    // home the settle drops rail focus, so the next Tab rests.
    assert_eq!(app.focused(), None);
    app.on_key(Key::Tab, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), None);
}

#[test]
fn tab_with_no_panel_or_rail_keeps_focus() {
    let mut app = groups(3, 60, 24);
    key(&mut app, Key::BackTab);
    let group = app.focused().expect("focus");
    assert_eq!(key(&mut app, Key::Tab), Effect::None);
    assert_eq!(app.focused(), Some(group));
}

#[test]
fn tab_in_the_input_box_does_what_it_did() {
    let mut app = groups(1, 60, 24);
    assert_eq!(key(&mut app, Key::Tab), Effect::None);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_frame_without_the_focused_target_returns_focus() {
    let mut app = attached(60, 24);
    let badge = Hit {
        id: TargetId::Badge,
        rect: Rect::new(0, 5, 10, 1),
    };
    app.drawn(&[badge]);
    assert_eq!(app.on_key(Key::BackTab, now()), Effect::None);
    assert!(!app.drawn(&[badge]));
    assert_eq!(app.focused(), Some(TargetId::Badge));
    assert!(app.drawn(&[]));
    assert_eq!(app.focused(), None);
    assert!(!app.drawn(&[]));
}

#[test]
fn the_second_ctrl_c_quits_while_navigating() {
    let mut app = groups(1, 60, 24);
    frame(&mut app);
    app.on_key(Key::BackTab, now());
    frame(&mut app);
    let group = app.focused().expect("focus");
    let at = now();
    assert_eq!(app.on_key(Key::CtrlC, at), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, at), Effect::Quit);
    assert_eq!(app.focused(), Some(group));
}

#[test]
fn an_approval_that_opens_while_navigating_still_gets_its_edits() {
    let mut first = groups(1, 60, 24);
    let mut second = groups(1, 60, 24);
    frame(&mut first);
    frame(&mut second);
    key(&mut first, Key::BackTab);
    let group = first.focused().expect("focus");
    first.on_line(permission_requested());
    second.on_line(permission_requested());
    assert!(first.panel().is_some());
    let before = first.panel();
    first.on_edit(Edit::Paste("why".to_owned()));
    second.on_edit(Edit::Paste("why".to_owned()));
    first.on_edit(Edit::Left);
    second.on_edit(Edit::Left);
    assert_eq!(first.panel(), second.panel());
    assert_ne!(first.panel(), before);
    assert_eq!(first.focused(), Some(group));
}

#[test]
fn shift_tab_with_a_completion_panel_open_stays_in_the_input_box() {
    let mut app = groups(1, 60, 24);
    key(&mut app, Key::Char('/'));
    assert!(app.completions().is_some());
    assert_eq!(key(&mut app, Key::BackTab), Effect::None);
    assert_eq!(app.focused(), None);
    assert!(app.completions().is_some());
}

#[test]
fn a_wrapped_item_is_revealed_whole() {
    // An open ledger's long call row wraps at width 12; a group summary
    // is one row, whatever runs, so the call row stands in for the
    // wrapped item the navigation must reveal whole.
    let mut app = attached(12, 8);
    app.on_line(envelope(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "shell",
            "arguments": {"command": "cargo test --workspace --all-targets"}}),
        Some("a_1"),
    ));
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_1"),
    ));
    key(&mut app, Key::CtrlO);
    let rows_of = |at: usize| {
        app.lines()
            .get(at)
            .map(|text| crate::view::rows(text.clone(), 12))
            .unwrap_or(0)
    };
    // An open ledger's long call row splits inside the padding, so every
    // stop is one row; navigating still lands each stop whole.
    let mut stops = 0;
    for (line, _) in app.targets() {
        let rows = rows_of(line);
        if app
            .lines()
            .get(line)
            .is_some_and(|text| text.to_string().starts_with(" ●"))
        {
            assert_eq!(rows, 1, "the summary is one row");
        } else {
            assert_eq!(rows, 1, "line {line} draws whole, with {rows} rows");
            stops += 1;
        }
    }
    assert!(stops > 0, "a split call row");
    let ids = group_ids(&app);
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), ids.last().copied());
    let first = app.items().first().copied().expect("an item");
    while app.focused() != Some(first.1) {
        let before = app.top();
        key(&mut app, Key::Up);
        if app.top() != before {
            let stops = frame(&mut app);
            let focused = app.focused().expect("focus");
            let rows = app
                .screen
                .pages()
                .focus_items()
                .into_iter()
                .find(|(_, _, item)| *item == focused)
                .map(|(_, rows, _)| rows)
                .expect("the focused stop is indexed");
            assert_eq!(rect_of(&stops, focused).height as usize, rows);
        }
    }
}

#[test]
fn a_taller_item_is_revealed_from_its_top() {
    // At width 12 a group line wraps to two rows, taller than the one
    // conversation row of a 12x2 screen.
    let mut app = groups(8, 12, 2);
    assert_eq!(app.conversation_height(), 1);
    key(&mut app, Key::BackTab);
    let first = app.items().first().copied().expect("an item");
    while app.focused() != Some(first.1) {
        key(&mut app, Key::Up);
    }
    let stops = frame(&mut app);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    assert_eq!(rect_of(&stops, TargetId::Turn(0)).y, 0);
    assert_eq!(
        rect_of(&stops, TargetId::Turn(0)).height as usize,
        app.conversation_height()
    );
}

#[test]
fn a_screen_with_no_conversation_rows_never_keeps_focus() {
    let mut app = groups(3, 60, 1);
    assert_eq!(app.conversation_height(), 0);
    let top = app.top();
    key(&mut app, Key::BackTab);
    frame(&mut app);
    assert_eq!(app.focused(), None);
    assert_eq!(app.top(), top);
}

#[test]
fn reveal_past_the_last_line_changes_nothing() {
    let mut app = attached(60, 6);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    for i in 1..=5 {
        app.on_line(envelope(
            "assistant_message_delta",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
        app.on_line(envelope(
            "text_completed",
            serde_json::json!({"text": format!("reply {i}")}),
            Some(format!("m_{i}").as_str()),
        ));
    }
    // Five one-row replies and the prompt at height 6: three conversation
    // rows, scrolled to the top.
    app.screen.jump(0);
    let len = app.lines().len();
    app.reveal(len);
    assert_eq!(app.top(), Some(0));
}

#[test]
fn y_copies_the_focused_line_and_shows_copied() {
    let mut app = groups(1, 60, 24);
    key(&mut app, Key::BackTab);
    let text = screen(&app)
        .lines()
        .find(|row| row.contains("Read 1 file"))
        .expect("the group row")
        .to_owned();
    assert_eq!(key(&mut app, Key::Char('y')), Effect::Copy(text));
    assert!(app.copied());
    key(&mut app, Key::Char('a'));
    assert!(!app.copied());
}

/// An app whose draft holds one 12-line paste token.
fn pasted() -> App {
    let mut app = attached(60, 24);
    let text = (1..=12)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.on_edit(Edit::Paste(text));
    frame(&mut app);
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), Some(TargetId::Token(1)));
    app
}

#[test]
fn y_on_a_paste_token_copies_its_text() {
    let mut app = pasted();
    let text = (1..=12)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(key(&mut app, Key::Char('y')), Effect::Copy(text));
}

#[test]
fn y_on_a_notice_or_a_steering_row_copies_its_text() {
    let mut app = attached(60, 24);
    app.notices.push("extension_failed", "Saved.".to_owned());
    app.on_line(steering_queue(&[("fix it", Some("c_1"))]));
    let targets = vec![
        Hit {
            id: TargetId::Notice(0),
            rect: Rect::new(10, 0, 20, 1),
        },
        Hit {
            id: TargetId::Steering(0),
            rect: Rect::new(0, 5, 10, 1),
        },
    ];
    app.drawn(&targets);
    app.on_key(Key::BackTab, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Steering(0)));
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("fix it".to_owned())
    );
    app.drawn(&targets);
    app.on_key(Key::Up, now());
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Notice(0)));
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("Saved.".to_owned())
    );
}

#[test]
fn y_and_ctrl_g_on_a_control_do_nothing() {
    for id in [
        TargetId::Badge,
        TargetId::NewBelow,
        TargetId::DismissNotice(0),
        TargetId::DropSteering(0),
        TargetId::MoreNotices,
    ] {
        let mut app = attached(60, 24);
        let target = Hit {
            id,
            rect: Rect::new(0, 5, 10, 1),
        };
        app.drawn(&[target]);
        app.on_key(Key::BackTab, now());
        app.drawn(&[target]);
        assert_eq!(app.focused(), Some(id), "{id:?} focuses");
        assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
        assert_eq!(app.on_key(Key::CtrlG, now()), Effect::None);
        assert!(!app.copied());
    }
}

#[test]
fn ctrl_g_opens_the_focused_item_read_only() {
    let mut app = groups(1, 60, 24);
    key(&mut app, Key::BackTab);
    let text = screen(&app)
        .lines()
        .find(|row| row.contains("Read 1 file"))
        .expect("the group row")
        .to_owned();
    assert_eq!(
        app.on_key(Key::CtrlG, now()),
        Effect::Editor {
            target: crate::editor::Target::Item,
            text,
        }
    );
    let lines = app.lines();
    app.editor_returned(crate::editor::Target::Item, Ok("changed".to_owned()));
    assert_eq!(app.input().expand(), "");
    assert_eq!(app.lines(), lines);
    app.editor_returned(
        crate::editor::Target::Item,
        Err(crate::editor::NO_EDITOR.to_owned()),
    );
    assert_eq!(app.notice(), Some(crate::editor::NO_EDITOR));
}

#[test]
fn ctrl_g_on_a_focused_paste_token_edits_the_token() {
    let mut app = pasted();
    let text = (1..=12)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        app.on_key(Key::CtrlG, now()),
        Effect::Editor {
            target: crate::editor::Target::Token(1),
            text,
        }
    );
}

#[test]
fn y_on_a_code_blocks_copy_copies_its_code() {
    let mut app = attached(60, 24);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    let code = "```rust\nlet a = 1;\n```";
    app.on_line(envelope(
        "assistant_message_delta",
        serde_json::json!({"text": code}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "text_completed",
        serde_json::json!({"text": code}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    frame(&mut app);
    key(&mut app, Key::BackTab);
    let copy = app
        .focused()
        .expect("the code block's copy stop is the newest item");
    assert!(matches!(copy, TargetId::Line(Target::Copy { .. })));
    let clicked = app.on_click(copy);
    key(&mut app, Key::BackTab);
    assert_eq!(app.on_key(Key::Char('y'), now()), clicked);
    assert!(matches!(clicked, Effect::Copy(_)));
}

#[test]
fn the_overlay_cross_closes_the_key_map_first() {
    let mut app = attached(60, 12);
    app.notices.push("extension_failed", "Saved.".to_owned());
    app.open_notice(0);
    key(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    assert!(app.notice_overlay().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert_eq!(app.keymap_top(), None);
    assert!(app.notice_overlay().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert_eq!(app.notice_overlay(), None);
}

#[test]
fn the_overlay_cross_closes_the_notice_overlay() {
    let mut app = attached(60, 12);
    app.notices.push("extension_failed", "Saved.".to_owned());
    app.open_notice(0);
    assert!(app.notice_overlay().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert_eq!(app.notice_overlay(), None);
}

/// A turn with prompt "go", reply "one", one read group and reply
/// "two", closed.
fn replied_turn() -> App {
    let mut app = attached(60, 24);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    for (action, text) in [("m_1", "one"), ("m_2", "two")] {
        app.on_line(envelope(
            "assistant_message_delta",
            serde_json::json!({"text": text}),
            Some(action),
        ));
        app.on_line(envelope(
            "text_completed",
            serde_json::json!({"text": text}),
            Some(action),
        ));
        if action == "m_1" {
            app.on_line(envelope(
                "tool_call_requested",
                serde_json::json!({"name": "read", "arguments": {"path": "src/1.rs"}}),
                Some("a_1"),
            ));
            app.on_line(envelope(
                "tool_call_completed",
                serde_json::json!({"status": "completed",
                    "content": [{"type": "text", "text": "ok"}]}),
                Some("a_1"),
            ));
        }
    }
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    app
}

/// Focuses turn `at` from the newest item: one Up per item above it.
fn focus_turn(app: &mut App, at: usize) {
    key(app, Key::BackTab);
    for _ in 0..16 {
        if app.focused() == Some(TargetId::Turn(at)) {
            return;
        }
        key(app, Key::Up);
    }
    panic!("never focused turn {at}");
}

#[test]
fn a_turn_is_a_stop_before_its_own_group_lines() {
    let mut app = groups(1, 60, 24);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/2.rs"}}),
        Some("a_2"),
    ));
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_2"),
    ));
    app.on_line(envelope(
        "assistant_message_delta",
        serde_json::json!({"text": "reply 2"}),
        Some("m_2"),
    ));
    app.on_line(envelope(
        "text_completed",
        serde_json::json!({"text": "reply 2"}),
        Some("m_2"),
    ));
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    assert_eq!(app.targets().len(), 2);
    let stops = frame(&mut app);
    let ids = group_ids(&app);
    assert_eq!(
        crate::focus::order(&stops, &app.regions, crate::focus::Area::Conversation),
        vec![
            TargetId::Turn(0),
            ids.first().copied().expect("a group"),
            TargetId::Turn(1),
            ids.get(1).copied().expect("a group"),
        ]
    );
}

#[test]
fn y_on_a_turn_copies_its_prompt_and_replies() {
    let mut app = replied_turn();
    focus_turn(&mut app, 0);
    // The rows with no target of their own: the prompt, the time under
    // it, the replies and the closing line, with the blank rows between
    // them; the group's own row is its line's text.
    assert_eq!(
        key(&mut app, Key::Char('y')),
        Effect::Copy("\ngo\nyou 00:00\n\none\n\n\ntwo\n\n▣ completed · 1 call".to_owned())
    );
}

/// An attached app whose finished turn holds a session message from
/// the sender, named `worker` by a direct status, its text, one image
/// and a reply.
fn turn_with_a_named_sender_message() -> App {
    let mut app = attached(60, 24);
    app.on_line(sender_status("worker", true));
    app.on_line(envelope(
        "turn_started",
        serde_json::json!({"input": [
            {"type": "message", "source": "driver", "content": [{"type": "text", "text": "go"}]},
            {"type": "message", "source": "session", "from_session_id": SENDER,
             "content": [{"type": "text", "text": "please check"},
                 {"type": "image", "path": "artifacts/a.png", "mime_type": "image/png",
                  "width": 10, "height": 8}]},
        ]}),
        None,
    ));
    app.on_line(envelope(
        "text_completed",
        serde_json::json!({"text": "a reply"}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    frame(&mut app);
    app
}

#[test]
fn a_directly_attached_terminal_labels_a_sender_by_its_latest_named_status() {
    let mut app = turn_with_a_named_sender_message();
    let shown = screen(&app);
    assert!(shown.contains("worker · 00:00"), "{shown}");
    // A rename reaches the drawn card: the label follows the latest
    // status (`docs/tui.md`, "Turns").
    app.on_line(sender_status("renamed", true));
    frame(&mut app);
    let shown = screen(&app);
    assert!(shown.contains("renamed · 00:00"), "{shown}");
    assert!(!shown.contains("worker · 00:00"), "{shown}");
}

#[test]
fn an_unnamed_or_empty_named_status_falls_back_to_the_sender_id() {
    for (name, named) in [("first prompt row", false), ("", true)] {
        let mut app = attached(60, 24);
        app.on_line(sender_status(name, named));
        app.on_line(envelope(
            "turn_started",
            serde_json::json!({"input": [
                {"type": "message", "source": "driver", "content": [{"type": "text", "text": "go"}]},
                {"type": "message", "source": "session", "from_session_id": SENDER,
                 "content": [{"type": "text", "text": "please check"}]},
            ]}),
            None,
        ));
        app.on_line(envelope(
            "text_completed",
            serde_json::json!({"text": "a reply"}),
            Some("m_1"),
        ));
        app.on_line(envelope(
            "turn_completed",
            serde_json::json!({"outcome": "completed"}),
            None,
        ));
        frame(&mut app);
        let shown = screen(&app);
        assert!(
            shown.contains(&format!("{SENDER} · 00:00")),
            "the sender id labels the message: {shown}"
        );
        assert!(!shown.contains("first prompt row"), "{shown}");
    }
}

#[test]
fn y_on_a_turn_with_a_session_message_copies_the_label_time_and_text() {
    let mut app = turn_with_a_named_sender_message();
    focus_turn(&mut app, 0);
    let Effect::Copy(text) = key(&mut app, Key::Char('y')) else {
        panic!("the whole turn copies")
    };
    // The label and time, then the body, in draw order; the labelled
    // rule is decoration, and the image copies through its own stop
    // (`docs/tui.md`, "Turns"; `docs/tui.md`, "Selection and copy").
    let label = text
        .find("worker · 00:00")
        .unwrap_or_else(|| panic!("the label: {text}"));
    let body = text
        .find("please check")
        .unwrap_or_else(|| panic!("the body: {text}"));
    let close = text
        .find("▣ completed")
        .unwrap_or_else(|| panic!("the closing line: {text}"));
    assert!(label < body && body < close, "{text}");
    assert!(!text.contains('─'), "the rule is decoration: {text}");
    assert!(
        !text.contains("a.png"),
        "the image copies through its own stop: {text}"
    );
}

#[test]
fn y_on_a_message_image_copies_its_visible_label() {
    let mut app = turn_with_a_named_sender_message();
    app.focus = Some(TargetId::Line(Target::Image(1)));
    assert_eq!(
        key(&mut app, Key::Char('y')),
        Effect::Copy("▣ a.png · 10×8".to_owned())
    );
}

#[test]
fn stepping_up_from_a_turns_first_group_reaches_the_turn_then_the_turn_before() {
    let mut app = groups(1, 60, 6);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/2.rs"}}),
        Some("a_2"),
    ));
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_2"),
    ));
    app.on_line(envelope(
        "assistant_message_delta",
        serde_json::json!({"text": "reply 2"}),
        Some("m_2"),
    ));
    app.on_line(envelope(
        "text_completed",
        serde_json::json!({"text": "reply 2"}),
        Some("m_2"),
    ));
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    assert_eq!(app.targets().len(), 2);
    let ids = group_ids(&app);
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), ids.get(1).copied());
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), Some(TargetId::Turn(1)));
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), ids.first().copied());
    assert!(app.top().is_some(), "scrolling reached the first group");
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
}

#[test]
fn enter_on_a_turn_does_nothing() {
    let mut app = replied_turn();
    let lines = app.lines();
    focus_turn(&mut app, 0);
    assert_eq!(key(&mut app, Key::Enter), Effect::None);
    assert_eq!(app.lines(), lines);
}

#[test]
fn the_notice_overlay_hides_the_turn_stops() {
    let mut app = groups(2, 60, 24);
    app.notices.push("extension_failed", "All.".to_owned());
    app.open_notice(0);
    let stops = frame(&mut app);
    assert!(
        stops
            .iter()
            .all(|target| !matches!(target.id, TargetId::Turn(_) | TargetId::Line(_))),
        "no conversation stop shows under the overlay"
    );
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), None);
}

/// One sequenced line of a session whose first turn spans pages.
fn seq_line(seq: u64, kind: &str, payload: serde_json::Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: seq.saturating_mul(1_000),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A session whose first turn runs 40 steps, so it spans pages, then a
/// short second turn so the first turn leaves the resident window.
fn spanning_session() -> Vec<Line> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, payload: serde_json::Value, action: Option<String>| {
        let action = action.as_deref();
        lines.push(seq_line(seq, kind, payload, action));
        seq = seq.saturating_add(1);
    };
    push(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    );
    for step in 0..40 {
        let message = format!("m_{step}");
        push("step_started", serde_json::json!({}), None);
        push(
            "assistant_message_started",
            serde_json::json!({}),
            Some(message.clone()),
        );
        push(
            "text_completed",
            serde_json::json!({"text": format!("reply {step}")}),
            Some(message.clone()),
        );
        push(
            "assistant_message_completed",
            serde_json::json!({"outcome": "completed"}),
            Some(message),
        );
    }
    push(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    );
    push(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "next"}]}]}),
        None,
    );
    push("step_started", serde_json::json!({}), None);
    push(
        "assistant_message_started",
        serde_json::json!({}),
        Some("m_last".to_owned()),
    );
    push(
        "text_completed",
        serde_json::json!({"text": "last reply"}),
        Some("m_last".to_owned()),
    );
    push(
        "assistant_message_completed",
        serde_json::json!({"outcome": "completed"}),
        Some("m_last".to_owned()),
    );
    push(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    );
    lines
}

/// The durable envelopes behind `lines`, as `history` answers them.
fn envelopes_of(lines: &[Line]) -> Vec<contract::Envelope> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Session(envelope) => Some(envelope.clone()),
            Line::Hub(_) => None,
        })
        .collect()
}

/// Loads every page `app` needs from `envelopes`, as the frame loop does.
fn load_needed(app: &mut App, envelopes: &[contract::Envelope]) {
    while let Some((session, range)) = app.needs().first().cloned() {
        let chunk: Vec<contract::Envelope> = envelopes
            .iter()
            .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
            .cloned()
            .collect();
        assert!(!chunk.is_empty(), "no lines for {range:?}");
        app.load(&session, chunk);
        assert_ne!(
            app.needs().first(),
            Some(&(session, range)),
            "loading did not settle"
        );
    }
}

/// `lines` open in an app of `height`, with every needed page loaded.
fn spanning_app(lines: &[Line], envelopes: &[contract::Envelope], height: u16) -> App {
    let mut app = attached(60, height);
    for line in lines {
        app.on_line(line.clone());
    }
    load_needed(&mut app, envelopes);
    frame(&mut app);
    app
}

/// The whole text of turn 0, from an app tall enough to hold every page.
fn whole_turn_zero(lines: &[Line], envelopes: &[contract::Envelope]) -> String {
    let mut app = spanning_app(lines, envelopes, 200);
    assert!(app.pages().part(0).is_some(), "the tall app drops nothing");
    app.focus = Some(TargetId::Turn(0));
    match app.on_key(Key::Char('y'), now()) {
        Effect::Copy(text) => text,
        Effect::None
        | Effect::Send(_)
        | Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("the whole turn copies"),
    }
}

#[test]
fn y_on_a_turn_spanning_dropped_pages_loads_the_whole_turn() {
    let lines = spanning_session();
    let envelopes = envelopes_of(&lines);
    let want = whole_turn_zero(&lines, &envelopes);
    assert!(want.contains("reply 0"), "{want:?}");
    assert!(want.contains("reply 39"), "{want:?}");
    let mut app = spanning_app(&lines, &envelopes, 8);
    assert!(app.pages().index().pages().len() > 2, "too few pages");
    assert!(
        app.pages().part(0).is_none(),
        "the first page stays resident"
    );
    app.focus = Some(TargetId::Turn(0));
    // The dropped fragments are not copied in part: the first press asks
    // for them and says so.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    assert_eq!(app.notice(), Some("Loading history…"));
    assert!(
        !app.needs().is_empty(),
        "nothing asked for the dropped pages"
    );
    load_needed(&mut app, &envelopes);
    assert!(
        app.pages().part(0).is_some(),
        "the dropped page did not load"
    );
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::Copy(want));
}

/// Turn 0's dropped pages pinned by a first y press, with the pinned
/// count from before the press to return to.
fn pending_copy() -> (App, Vec<contract::Envelope>, usize) {
    let lines = spanning_session();
    let envelopes = envelopes_of(&lines);
    let mut app = spanning_app(&lines, &envelopes, 8);
    assert!(
        app.pages().part(0).is_none(),
        "the first page stays resident"
    );
    let prior = app.pages().pinned();
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    assert_eq!(app.notice(), Some("Loading history…"));
    assert!(
        app.pages().pinned() > prior,
        "nothing pinned the dropped pages"
    );
    (app, envelopes, prior)
}

#[test]
fn esc_releases_a_whole_turn_copy_waiting_on_dropped_pages() {
    let (mut app, _, prior) = pending_copy();
    // With the notice open Esc closes it and keeps focus: only the Esc
    // path releases the pinned pages.
    app.open_notice(0);
    app.on_key(Key::Esc, now());
    assert_eq!(app.focused(), Some(TargetId::Turn(0)));
    assert_eq!(app.pages().pinned(), prior);
}

#[test]
fn moving_focus_releases_a_whole_turn_copy_waiting_on_dropped_pages() {
    let (mut app, _, prior) = pending_copy();
    app.on_key(Key::BackTab, now());
    assert_ne!(app.focused(), Some(TargetId::Turn(0)));
    assert_eq!(app.pages().pinned(), prior);
}

#[test]
fn a_failed_history_load_releases_a_whole_turn_copy_waiting_on_dropped_pages() {
    let (mut app, _, prior) = pending_copy();
    let needs: Vec<_> = app.needs().into_iter().collect();
    assert!(!needs.is_empty(), "nothing asked for the dropped pages");
    for (session, range) in &needs {
        app.load_failed(session, range, "boom");
    }
    assert_eq!(app.pages().pinned(), prior);
}

#[test]
fn requesting_another_turn_releases_the_pending_whole_turn_copy() {
    let (mut app, _, prior) = pending_copy();
    app.focus = Some(TargetId::Turn(1));
    // No settle runs here, so only the replacement branch can release
    // turn 0's pages.
    let text = app.turn_text(1);
    assert!(text.is_some(), "the resident turn has text");
    assert_eq!(app.pages().pinned(), prior);
}

#[test]
fn ctrl_g_on_a_turn_spanning_dropped_pages_opens_the_whole_turn() {
    let lines = spanning_session();
    let envelopes = envelopes_of(&lines);
    let want = whole_turn_zero(&lines, &envelopes);
    let mut app = spanning_app(&lines, &envelopes, 8);
    assert!(
        app.pages().part(0).is_none(),
        "the first page stays resident"
    );
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(app.on_key(Key::CtrlG, now()), Effect::None);
    assert_eq!(app.notice(), Some("Loading history…"));
    load_needed(&mut app, &envelopes);
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(
        app.on_key(Key::CtrlG, now()),
        Effect::Editor {
            target: crate::editor::Target::Item,
            text: want
        },
    );
}

#[test]
fn a_second_y_on_a_pending_turn_pins_nothing_twice() {
    let (mut app, envelopes, prior) = pending_copy();
    let pinned = app.pages().pinned();
    app.focus = Some(TargetId::Turn(0));
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    assert_eq!(
        app.pages().pinned(),
        pinned,
        "the second press pinned a page again"
    );
    load_needed(&mut app, &envelopes);
    app.focus = Some(TargetId::Turn(0));
    assert!(matches!(app.on_key(Key::Char('y'), now()), Effect::Copy(_)));
    assert_eq!(
        app.pages().pinned(),
        prior,
        "the copy unpins what it pinned"
    );
}

#[test]
fn a_turn_copy_with_nothing_missing_pins_nothing() {
    let (mut app, _, prior) = pending_copy();
    // Turn 1 is resident: copying it pins nothing, and replaces turn 0's
    // pending copy, whose pins go with it.
    app.focus = Some(TargetId::Turn(1));
    assert!(app.turn_text(1).is_some());
    assert_eq!(app.pages().pinned(), prior);
    assert!(app.pending_turn.is_none());
}

/// An app with the search panel open on `query`, the draft kept.
fn searching(query: &str) -> App {
    let mut app = attached(80, 24);
    key(&mut app, Key::Char('d'));
    key(&mut app, Key::Char('r'));
    key(&mut app, Key::CtrlR);
    assert!(app.completions().is_some());
    for ch in query.chars() {
        key(&mut app, Key::Char(ch));
    }
    app
}

/// The drawn CloseOverlay target, read from the frame.
fn cross(app: &mut App) -> Hit {
    frame(app)
        .into_iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the panel draws its cross")
}

#[test]
fn the_overlay_cross_closes_the_search_panel() {
    let mut app = searching("fi");
    let before = app.input().expand();
    let id = cross(&mut app).id;
    assert_eq!(app.on_click(id), Effect::None);
    assert!(app.completions().is_none());
    assert_eq!(app.input().expand(), before);
    // The next draw holds no panel.
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let shown = crate::view::text({
        crate::view::render(&app, area, &mut buf, None);
        &buf
    });
    assert!(!shown.contains("Prompt history"), "{shown}");
}

#[test]
fn the_overlay_cross_closes_the_key_map_before_the_search_panel() {
    let mut app = searching("fi");
    key(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert!(app.keymap_top().is_none());
    assert!(app.completions().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert!(app.completions().is_none());
}

#[test]
fn the_overlay_cross_closes_the_search_panel_before_the_notice_overlay() {
    let mut app = searching("fi");
    app.notices.push("extension_failed", "Saved.".to_owned());
    app.open_notice(0);
    assert!(app.notice_overlay().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert!(app.completions().is_none());
    assert!(app.notice_overlay().is_some());
    app.on_click(TargetId::CloseOverlay);
    assert_eq!(app.notice_overlay(), None);
}

#[test]
fn the_overlay_cross_with_nothing_open_changes_nothing() {
    let mut app = attached(80, 24);
    assert_eq!(app.on_click(TargetId::CloseOverlay), Effect::None);
    assert!(app.completions().is_none());
    assert_eq!(app.notice_overlay(), None);
}

#[test]
fn the_overlay_cross_closes_the_search_panel_on_home() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(80, 24);
    key(&mut app, Key::CtrlR);
    assert!(app.completions().is_some());
    let id = cross(&mut app).id;
    assert_eq!(app.on_click(id), Effect::None);
    assert!(app.completions().is_none());
}

/// An app with one group holding two read calls, the first with `lines`
/// lines of output: the ledger and the first call opened.
fn opened_call(lines: usize) -> (App, Target, Target, Target) {
    let mut app = attached(60, 10);
    let input = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    app.on_line(envelope("turn_started", input, None));
    app.on_line(envelope(
        "assistant_message_started",
        serde_json::json!({}),
        Some("a_m"),
    ));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/0.rs"}}),
        Some("a_c0"),
    ));
    let detail: Vec<String> = (1..=lines).map(|n| format!("L{n:04} of call")).collect();
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": detail.join("\n")}]}),
        Some("a_c0"),
    ));
    app.on_line(envelope(
        "tool_call_requested",
        serde_json::json!({"name": "read", "arguments": {"path": "src/1.rs"}}),
        Some("a_c1"),
    ));
    app.on_line(envelope(
        "tool_call_completed",
        serde_json::json!({"status": "completed",
            "content": [{"type": "text", "text": "ok"}]}),
        Some("a_c1"),
    ));
    let tail: Vec<String> = (1..=12).map(|n| format!("tail {n}")).collect();
    app.on_line(envelope(
        "assistant_message_delta",
        serde_json::json!({"text": tail.join("\n")}),
        Some("a_m"),
    ));
    app.on_line(envelope(
        "text_completed",
        serde_json::json!({"text": tail.join("\n")}),
        Some("a_m"),
    ));
    app.on_line(envelope(
        "assistant_message_completed",
        serde_json::json!({"outcome": "completed"}),
        Some("a_m"),
    ));
    app.on_line(envelope(
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
        None,
    ));
    let targets = app.targets();
    let group = targets
        .iter()
        .find_map(|(_, target)| matches!(target, Target::Group(_)).then_some(*target))
        .expect("a group target");
    app.open(group);
    let mut calls = app
        .targets()
        .into_iter()
        .filter_map(|(_, target)| matches!(target, Target::Call(_)).then_some(target));
    let call = calls.next().expect("a first call");
    let second = calls.next().expect("a second call");
    app.open(call);
    (app, group, call, second)
}

/// The absolute row of `line` in the open page's text: the first wrapped
/// row of the first text with its target.
fn text_row(app: &App, target: Target) -> usize {
    let (rows, _) = app.pages().page_text(0).expect("the open page");
    let width = app.pages().wrap_width();
    let mut row = 0usize;
    for (line, line_target) in &rows {
        if *line_target == Some(target) {
            return row;
        }
        row += crate::view::rows(line.clone(), width);
    }
    panic!("no row for {target:?}");
}

#[test]
fn focus_steps_under_the_pin() {
    let (mut app, _, call, second) = opened_call(30);
    frame(&mut app);
    let more = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::CallView(_)).then_some(target))
        .expect("a more target");
    let more_row = text_row(&app, more);
    // The top sits on the more row, so the call pins row 0 over it: the
    // covered row keeps no hit and is no focus stop.
    app.jump(more_row);
    let stops = frame(&mut app);
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "the call pins the top row"
    );
    assert!(
        !stops.iter().any(|stop| stop.id == TargetId::Line(more)),
        "the covered row is no stop: {:?}",
        stops
            .iter()
            .map(|stop| (stop.id, stop.rect))
            .collect::<Vec<_>>()
    );
    // BackTab focuses the newest item, the second call; stepping up
    // reaches the covered more row and reveals it under the pin.
    key(&mut app, Key::BackTab);
    assert_eq!(app.focused(), Some(TargetId::Line(second)));
    let focused = TargetId::Line(more);
    key(&mut app, Key::Up);
    assert_eq!(app.focused(), Some(focused));
    assert_eq!(
        app.top(),
        Some(more_row.saturating_sub(1)),
        "the more row lands one row below the top"
    );
    let stops = frame(&mut app);
    assert_eq!(
        rect_of(&stops, focused).y,
        1,
        "the more row is under the pin"
    );
    assert_eq!(
        app.pinned_row().map(|pin| pin.target),
        Some(call),
        "the pin stays on the call"
    );
}

#[test]
fn turn_end_clears_switch_pending_notices() {
    let mut app = attached(80, 24);
    app.phase = super::super::Phase::Attached {
        session: contract::SessionId(SESSION.to_owned()),
        busy: true,
    };
    app.push_notice("switch_pending", "a applies when this turn ends".to_owned());
    app.push_notice("switch_pending", "b applies when this turn ends".to_owned());
    assert_eq!(app.notices().len(), 2);
    // Staying busy, or already idle, ends nothing.
    app.set_busy(true);
    assert_eq!(app.notices().len(), 2);
    // The true-to-false fold clears the applied shares.
    app.set_busy(false);
    assert!(app.notices().is_empty());
    // Clearing twice is a no-op.
    app.set_busy(false);
    assert!(app.notices().is_empty());
}

#[test]
fn one_frames_queued_notices_group_by_code() {
    // Two queued `io_failed` notices in one frame batch stack one
    // grouped row: the count and the first item up front, every item
    // listed.
    let mut app = groups(1, 80, 24);
    app.screen
        .pages()
        .ext
        .cache
        .queue_notice(("io_failed".to_owned(), "a first read failed".to_owned()));
    app.screen
        .pages()
        .ext
        .cache
        .queue_notice(("io_failed".to_owned(), "a second read failed".to_owned()));
    assert!(app.drain_renderer_failures());
    let rows: Vec<(String, String)> = app
        .notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => Some((code, text)),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "io_failed");
    assert!(
        rows[0]
            .1
            .starts_with("2 reads failed: a first read failed\n"),
        "{}",
        rows[0].1
    );
    assert!(rows[0].1.contains("- a first read failed"), "{}", rows[0].1);
    assert!(
        rows[0].1.contains("- a second read failed"),
        "{}",
        rows[0].1
    );
}

#[test]
fn loaded_history_clears_its_loading_notice() {
    let (mut app, envelopes, _) = pending_copy();
    assert_eq!(app.notice(), Some("Loading history…"));
    load_needed(&mut app, &envelopes);
    app.focus = Some(TargetId::Turn(0));
    // The landed copy answers the press, and the wait's notice goes
    // with the pending turn.
    let _ = app.on_key(Key::Char('y'), now());
    assert!(app.notices().is_empty(), "{:?}", app.notices());
}
