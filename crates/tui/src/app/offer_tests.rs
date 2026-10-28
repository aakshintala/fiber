//! Tests for the repository offer on the app: what opens it, the keys and
//! edits it takes, and the reply it sends.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::app::testkit::{SESSION as S_A, now, sent, session_line};
use crate::app::{App, Effect};
use crate::keys::{Edit, Key, Mouse, MouseKind};
use crate::link::Line;

const S_B: &str = "s_bbbbbbbbbbbbbbbb";
const BADGE_ONE: &str = "  ⚠ 1 approvals waiting · click to reopen";

/// An app connected to the hub and attached to [`S_A`].
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app.attach(contract::SessionId(S_A.to_owned()));
    app
}

/// An offer `r_1` of three MCP servers from `session`.
fn offer(session: &str) -> Line {
    let items: Vec<Value> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            json!({"kind": "mcp_server", "name": name, "hash": "h", "required": false,
                "summary": format!("MCP server: {name}")})
        })
        .collect();
    session_line(
        session,
        "repository_code_offered",
        json!({"request_id": "r_1", "items": items}),
        None,
    )
}

/// An offer `r_1` of ten MCP servers from `session`: tall enough that
/// the wheel moves its window three rows a step.
fn offer_many(session: &str) -> Line {
    let items: Vec<Value> = (0..10)
        .map(|n| {
            json!({"kind": "mcp_server", "name": format!("m{n}"), "hash": "h", "required": false,
                "summary": format!("MCP server: m{n}")})
        })
        .collect();
    session_line(
        session,
        "repository_code_offered",
        json!({"request_id": "r_1", "items": items}),
        None,
    )
}

/// An app attached to [`S_A`] with its offer open.
fn offered() -> App {
    let mut app = attached();
    assert!(app.on_line(offer(S_A)).is_empty());
    assert!(app.offer_open());
    app
}

/// Feeds the offered repository code lines, opening the offer.
fn offered_lines(app: &mut App) {
    assert!(app.on_line(offer(S_A)).is_empty());
    assert!(app.offer_open());
}

/// An approval request from [`S_A`].
fn approval(id: &str) -> Line {
    session_line(
        S_A,
        "permission_requested",
        json!({"request_id": id, "effects": ["executes"], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "global", "prefix": "p"}}),
        Some("a_1"),
    )
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, now())
}

/// Chooses approve, skip, never with the keys, then Enter on Send.
fn answer_with_keys(app: &mut App) -> Vec<Value> {
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(press(app, Key::Down), Effect::None);
    assert_eq!(press(app, Key::Down), Effect::None);
    assert_eq!(app.on_edit(Edit::Right), Effect::None);
    assert_eq!(press(app, Key::Down), Effect::None);
    sent(press(app, Key::Enter))
}

fn with_source() -> App {
    let mut app = attached();
    app.set_size(80, 24);
    app.on_line(session_line(S_A, "repository_code_offered", json!({"request_id": "r_1", "items": [{"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "hook", "files": ["tools/db.sh", "tools/b.sh"]}]}), None));
    app
}

fn source_text(app: &App) -> String {
    app.offer_rows(app.column_width())
        .unwrap()
        .0
        .iter()
        .map(|row| row.line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_source(app: &mut App) -> Value {
    let lines = sent(press(app, Key::Enter));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "offered_file");
    assert_eq!(lines[0]["session_id"], S_A);
    assert_eq!(
        lines[0]["args"],
        json!({"request_id": "r_1", "path": "tools/db.sh"})
    );
    lines[0].clone()
}

#[test]
fn enter_reads_source_and_steps_ignore_late_answers_without_changing_decisions() {
    let mut app = with_source();
    app.on_edit(Edit::Left);
    let first = open_source(&mut app);
    assert!(source_text(&app).contains("Loading…"));
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": first["id"], "result": {"text": "first source"}}),
        None,
    ));
    assert!(source_text(&app).contains("first source"));
    let next = sent(app.on_edit(Edit::Right));
    assert_eq!(next.len(), 1);
    assert_eq!(next[0]["args"]["path"], "tools/b.sh");
    assert!(sent(app.on_edit(Edit::Right)).is_empty());
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": first["id"], "result": {"text": "late source"}}),
        None,
    ));
    assert!(!source_text(&app).contains("late source"));
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": next[0]["id"], "result": {"text": "second source"}}),
        None,
    ));
    assert!(source_text(&app).contains("second source"));
    let previous = sent(app.on_edit(Edit::Left));
    assert_eq!(previous[0]["args"]["path"], "tools/db.sh");
    assert!(sent(app.on_edit(Edit::Left)).is_empty());
    press(&mut app, Key::Esc);
    assert!(app.offer_open());
    press(&mut app, Key::Down);
    let reply = sent(press(&mut app, Key::Enter));
    assert_eq!(reply[0]["args"]["decisions"], json!(["approve"]));
}

#[test]
fn an_answered_source_marks_new_output_only_for_its_request_while_scrolled_up() {
    for scrolled in [false, true] {
        for matching in [false, true] {
            let mut app = attached();
            app.set_size(80, 24);
            let history = "earlier output\n".repeat(60);
            app.on_line(session_line(
                S_A,
                "text_completed",
                json!({"text": history}),
                Some("a_history"),
            ));
            if scrolled {
                press(&mut app, Key::PageUp);
            }
            assert_eq!(app.top().is_some(), scrolled);
            assert!(!app.has_new());
            app.on_line(session_line(S_A, "repository_code_offered", json!({"request_id": "r_1", "items": [{"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "hook", "files": ["tools/db.sh"]}]}), None));
            let command = open_source(&mut app);
            let id = if matching {
                command["id"].clone()
            } else {
                json!("c_unrelated")
            };
            app.on_line(session_line(
                S_A,
                "command_accepted",
                json!({"command_id": id, "result": {"text": "new source"}}),
                None,
            ));
            assert_eq!(source_text(&app).contains("new source"), matching);
            press(&mut app, Key::Esc);
            press(&mut app, Key::Esc);
            let area = ratatui::layout::Rect::new(0, 0, 80, 24);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            crate::view::render(&app, area, &mut buffer, None);
            assert_eq!(
                crate::view::text(&buffer).contains("New messages below"),
                scrolled && matching
            );
        }
    }
}

#[test]
fn source_refusals_are_inline_from_the_session_or_hub_and_duplicates_are_ignored() {
    for hub in [false, true] {
        for (code, shown) in [
            ("invalid_arguments", "Read refused."),
            ("duplicate_command", "Loading…"),
        ] {
            let mut app = with_source();
            let command = open_source(&mut app);
            let payload =
                json!({"command_id": command["id"], "code": code, "message": "Read refused."});
            let line = if hub {
                Line::Hub(contract::HubLine {
                    kind: "command_rejected".into(),
                    ts: 0,
                    schema_version: 1,
                    payload: payload.as_object().unwrap().clone(),
                })
            } else {
                session_line(S_A, "command_rejected", payload, None)
            };
            app.on_line(line);
            assert!(source_text(&app).contains(shown));
            assert!(app.notice().is_none());
        }
    }
}

#[test]
fn reconnect_closes_source_and_replay_shows_the_offer_list() {
    let mut app = with_source();
    let Effect::Send(lines) = press(&mut app, Key::Enter) else {
        panic!("Enter requests the offered source");
    };
    let command: Value = serde_json::from_str(&lines[0]).unwrap();
    app.outbox.wrote(&lines[0]);
    assert!(app.offer.source_open());
    app.disconnected();
    let resent = app.on_line(Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    assert!(resent.contains(&lines[0]));
    assert!(!app.offer.source_open());
    assert!(!app.offer_open());
    app.on_line(session_line(S_A, "repository_code_offered", json!({"request_id": "r_1", "items": [{"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "hook", "files": ["tools/db.sh", "tools/b.sh"]}]}), None));
    assert!(app.offer_open());
    assert!(!app.offer.source_open());
    assert!(source_text(&app).contains("skip for this session"));
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": command["id"], "result": {"text": "late source"}}),
        None,
    ));
    assert!(!app.offer.source_open());
    assert!(!source_text(&app).contains("late source"));
    assert!(source_text(&app).contains("skip for this session"));
}

#[test]
fn source_escape_resolution_home_and_link_down_follow_the_offer() {
    let mut app = with_source();
    open_source(&mut app);
    press(&mut app, Key::Esc);
    assert!(app.offer_open());
    assert!(!source_text(&app).contains("Loading…"));
    press(&mut app, Key::Esc);
    assert!(!app.offer_open());
    let mut app = with_source();
    open_source(&mut app);
    app.on_line(session_line(
        S_A,
        "repository_code_resolved",
        json!({"request_id": "r_1", "decisions": ["skip"]}),
        None,
    ));
    assert!(!app.offer_open());
    let mut app = with_source();
    open_source(&mut app);
    app.go_home();
    assert!(!app.offer_open());
    let mut app = with_source();
    app.disconnected();
    assert!(sent(press(&mut app, Key::Enter)).is_empty());
    assert!(!source_text(&app).contains("Loading…"));
}

#[test]
fn source_escape_closes_notices_first_and_ctrl_c_still_clears_then_quits() {
    let mut app = with_source();
    open_source(&mut app);
    app.on_line(session_line(
        S_A,
        "notice",
        json!({"code": "io_failed", "message": "notice"}),
        None,
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    press(&mut app, Key::Esc);
    assert!(app.notice_overlay().is_none());
    assert!(source_text(&app).contains("Loading…"));
    app.draft.set("clear me");
    assert_eq!(press(&mut app, Key::CtrlC), Effect::None);
    assert_eq!(app.draft(), "");
    assert_eq!(press(&mut app, Key::CtrlC), Effect::None);
    assert_eq!(press(&mut app, Key::CtrlC), Effect::Quit);
}

#[test]
fn source_escape_closes_the_source_and_keeps_the_offer() {
    let mut app = with_source();
    open_source(&mut app);
    assert!(app.offer.source_open());
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.offer.source_open());
    assert!(app.offer.open(), "the first Esc leaves the offer up");
}

#[test]
fn pagedown_reaches_a_long_files_end_with_the_rail_and_panel_shown() {
    let mut app = with_source();
    app.set_size(120, 30);
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".into(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.attach(contract::SessionId(S_A.into()));
    app.chrome.show_rail();
    app.set_size(120, 30);
    let command = open_source(&mut app);
    let text = (0..300)
        .map(|n| format!("line {n:03} {}\n", "long text ".repeat(20)))
        .collect::<String>();
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": command["id"], "result": {"text": text}}),
        None,
    ));
    let width = app.column_width();
    assert!(width < 120);
    let height = app.conversation_height();
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(30));
    let mut reached = false;
    for _ in 0..2000 {
        assert!(
            !deadline.left().is_zero(),
            "PageDown reaches the last source page"
        );
        let (rows, top) = app.offer_rows(width).unwrap();
        let total: usize = rows.iter().map(|row| row.height(width)).sum();
        if top == total.saturating_sub(height) {
            assert!(
                rows.iter()
                    .skip(top)
                    .any(|row| row.line.to_string().contains("line 299"))
            );
            let area = ratatui::layout::Rect::new(0, 0, 120, 30);
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            crate::view::render(&app, area, &mut buffer, None);
            assert!(crate::view::text(&buffer).contains("line 299"));
            reached = true;
            break;
        }
        press(&mut app, Key::PageDown);
    }
    assert!(
        reached,
        "PageDown reaches the last wrapped row with a narrow conversation"
    );
}

#[test]
fn enter_on_an_item_with_no_files_shows_a_notice_and_sends_nothing() {
    let mut app = offered();
    assert!(sent(press(&mut app, Key::Enter)).is_empty());
    assert_eq!(app.notice(), Some("This item lists no files."));
}

#[test]
fn an_attached_sessions_offer_opens_the_view() {
    let app = offered();
    let rows = app.offer_rows(80).map(|(rows, _)| rows).unwrap_or_default();
    assert!(rows.len() > 3);
}

#[test]
fn another_sessions_offer_does_not_show() {
    let mut app = attached();
    assert!(app.on_line(offer(S_B)).is_empty());
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn the_keys_build_the_decisions_and_send_one_reply() {
    let mut app = offered();
    let lines = answer_with_keys(&mut app);
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap_or_default().to_owned();
    assert!(id.starts_with("c_"));
    assert_eq!(
        lines[0],
        json!({"id": id, "command": "reply", "session_id": S_A,
            "args": {"request_id": "r_1", "decisions": ["approve", "skip", "never"]}})
    );
    // Answered, the view closes and the badge does not count it.
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn no_reply_with_the_link_down() {
    let mut app = offered();
    app.disconnected();
    assert!(answer_with_keys(&mut app).is_empty());
    assert!(app.offer_open());
}

#[test]
fn a_rejected_reply_reopens_the_offer_with_its_choices() {
    let mut app = offered();
    let lines = answer_with_keys(&mut app);
    let rejected = session_line(
        S_A,
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "stale_request", "message": "Stale."}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.notice(), Some("Stale."));
    assert!(app.offer_open());
    assert_eq!(app.draft(), "");
    let again = sent(press(&mut app, Key::Enter));
    assert_eq!(
        again[0]["args"]["decisions"],
        json!(["approve", "skip", "never"])
    );
}

#[test]
fn a_failed_write_reopens_the_offer() {
    let mut app = offered();
    app.on_edit(Edit::Left);
    let Effect::Send(lines) = app.on_click(crate::mouse::TargetId::Offer(crate::offer::Spot::Send))
    else {
        panic!("Send sends");
    };
    assert!(!app.offer_open());
    app.write_failed(&lines);
    assert!(app.offer_open());
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn another_clients_resolution_closes_it() {
    let mut app = offered();
    let resolved = session_line(
        S_A,
        "repository_code_resolved",
        json!({"request_id": "r_1", "decisions": ["skip", "skip", "skip"]}),
        None,
    );
    assert!(app.on_line(resolved).is_empty());
    assert!(!app.offer_open());
    assert!(app.badge().is_none());
}

#[test]
fn the_badge_ignores_the_next_requests_bound_key() {
    let mut app = attached();
    let mut user = serde_json::Map::new();
    user.insert("next_request".to_owned(), json!("alt+n"));
    app.set_keys(crate::KeysSetup { user });
    assert!(app.on_line(offer(S_A)).is_empty());
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn the_badge_shows_the_same_without_a_bound_key() {
    let mut app = attached();
    let mut user = serde_json::Map::new();
    user.insert("next_request".to_owned(), json!([]));
    app.set_keys(crate::KeysSetup { user });
    assert!(app.on_line(offer(S_A)).is_empty());
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn the_badge_counts_it_after_esc() {
    let mut app = offered();
    assert!(app.badge().is_none());
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.offer_open());
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn the_badge_counts_it_with_a_put_aside_approval() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    app.on_line(approval("r_9"));
    assert!(app.panel().is_some());
    press(&mut app, Key::Esc);
    assert_eq!(
        app.badge().as_deref(),
        Some("  ⚠ 2 approvals waiting · click to reopen")
    );
    // ⌥A reopens the offer first.
    press(&mut app, Key::AltA);
    assert!(app.offer_open());
    assert!(app.panel().is_none());
}

#[test]
fn alt_a_reopens_it() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    assert_eq!(press(&mut app, Key::AltA), Effect::None);
    assert!(app.offer_open());
    assert!(app.notice().is_none());
}

#[test]
fn slash_approvals_reopens_the_offer() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    for ch in "/approvals".chars() {
        press(&mut app, Key::Char(ch));
    }
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(app.offer_open());
    assert!(app.notice().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn a_badge_click_reopens_it() {
    let mut app = offered();
    press(&mut app, Key::Esc);
    app.on_click(crate::mouse::TargetId::Badge);
    assert!(app.offer_open());
}

#[test]
fn the_close_target_puts_it_aside() {
    let mut app = offered();
    app.on_click(crate::mouse::TargetId::Offer(crate::offer::Spot::Close));
    assert!(!app.offer_open());
    assert_eq!(app.badge().as_deref(), Some(BADGE_ONE));
}

#[test]
fn esc_closes_the_notice_list_before_the_offer() {
    let mut app = offered();
    app.on_line(session_line(
        S_A,
        "notice",
        json!({"code": "internal", "message": "hello"}),
        None,
    ));
    app.open_more_notices();
    assert!(app.notice_overlay().is_some());
    press(&mut app, Key::Esc);
    assert!(app.notice_overlay().is_none());
    assert!(app.offer_open());
    press(&mut app, Key::Esc);
    assert!(!app.offer_open());
}

#[test]
fn with_the_approval_panel_open_keys_go_to_the_panel() {
    let mut app = offered();
    app.on_line(approval("r_9"));
    assert!(app.panel().is_some());
    press(&mut app, Key::Char('n'));
    press(&mut app, Key::Char('o'));
    let lines = sent(press(&mut app, Key::Enter));
    assert_eq!(lines[0]["args"]["request_id"], "r_9");
    assert_eq!(lines[0]["args"]["feedback"], "no");
    // The offer is still open, and answered next.
    assert!(app.offer_open());
}

#[test]
fn a_paste_with_the_approval_panel_open_goes_to_its_feedback() {
    let mut app = offered();
    app.on_line(approval("r_9"));
    assert_eq!(app.on_edit(Edit::Paste("why".to_owned())), Effect::None);
    let lines = sent(press(&mut app, Key::Enter));
    assert_eq!(lines[0]["args"]["feedback"], "why");
}

#[test]
fn f1_opens_the_key_map_over_it() {
    let mut app = offered();
    press(&mut app, Key::F1);
    assert!(app.keymap_top().is_some());
    press(&mut app, Key::Esc);
    assert!(app.keymap_top().is_none());
    assert!(app.offer_open());
}

#[test]
fn typed_characters_and_arrows_never_reach_the_draft() {
    let mut app = offered();
    for ch in "hi".chars() {
        press(&mut app, Key::Char(ch));
    }
    app.on_edit(Edit::Paste("x".to_owned()));
    app.on_edit(Edit::Left);
    press(&mut app, Key::Backspace);
    assert_eq!(app.draft(), "");
}

#[test]
fn going_home_drops_the_offer() {
    let mut app = offered();
    // The view takes typed keys, so it is put aside first.
    press(&mut app, Key::Esc);
    for ch in "/home".chars() {
        press(&mut app, Key::Char(ch));
    }
    press(&mut app, Key::Enter);
    assert!(app.session().is_none());
    assert!(app.badge().is_none());
    press(&mut app, Key::AltA);
    assert!(!app.offer_open());
}

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the offer takes.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// The text of the open offer's cursor row: the one its `▸` mark starts.
fn cursor_text(app: &App) -> String {
    let (rows, _) = app
        .offer_rows(app.column_width())
        .expect("the offer is open");
    rows.iter()
        .find_map(|row| {
            let mut text = String::new();
            for span in &row.line.spans {
                text.push_str(span.content.as_ref());
            }
            text.strip_prefix("▸ ").map(str::to_owned)
        })
        .expect("a cursor row")
}

/// The wheel over the offer with no source open shifts its rows three,
/// never the conversation under it (`docs/tui.md`, "Swapped views";
/// see #3578). Two Downs put the cursor on the third item, so the
/// window moves three rows under it while the choice stays.
#[test]
fn wheel_over_offer_moves_the_cursor_not_the_conversation() {
    let mut app = attached();
    app.set_size(80, 24);
    // Scroll first, while the wheel still reaches the conversation: the
    // offer covers it once its rows draw.
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert!(app.on_line(offer_many(S_A)).is_empty());
    assert!(app.offer_open());
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    press(&mut app, Key::Down);
    press(&mut app, Key::Down);
    assert!(cursor_text(&app).contains("MCP server m2"));
    let (_, offer_top) = app
        .offer_rows(app.column_width())
        .expect("the offer is open");
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    let (_, moved) = app
        .offer_rows(app.column_width())
        .expect("the offer is open");
    assert_eq!(
        moved,
        offer_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert!(
        cursor_text(&app).contains("MCP server m2"),
        "a cursor still on screen stays where it was"
    );
}

/// The wheel over the offer with no source open clamps a cursor that
/// leaves the window to the nearest shown row, and never chooses: the
/// cursor moves off the first item while no answer sends.
#[test]
fn wheel_over_offer_clamps_a_leaving_cursor_without_choosing() {
    let mut app = attached();
    app.set_size(80, 24);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    assert!(app.on_line(offer_many(S_A)).is_empty());
    assert!(app.offer_open());
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    assert!(cursor_text(&app).contains("MCP server m0"));
    for _ in 0..3 {
        assert_eq!(app.on_wheel(&wheel(10, 5, true)), Effect::None);
    }
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(
        !cursor_text(&app).contains("MCP server m0"),
        "the first item left the window, so the cursor followed"
    );
}

/// The wheel over the offer with its source open scrolls the source
/// three rows, never the conversation under it (`docs/tui.md`, "Swapped
/// views"; see #3578).
#[test]
fn wheel_over_offer_source_scrolls_the_source_not_the_conversation() {
    let mut app = attached();
    app.set_size(80, 24);
    // Scroll first, while the wheel still reaches the conversation: the
    // offer covers it once its rows draw.
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    app.on_line(session_line(S_A, "repository_code_offered", json!({"request_id": "r_1", "items": [{"kind": "hook", "name": "db", "hash": "h", "required": false, "summary": "hook", "files": ["tools/db.sh", "tools/b.sh"]}]}), None));
    assert!(app.offer_open());
    let request = open_source(&mut app);
    let text = (1..=30)
        .map(|n| format!("line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.on_line(session_line(
        S_A,
        "command_accepted",
        json!({"command_id": request["id"], "result": {"text": text}}),
        None,
    ));
    assert!(app.offer.source_open());
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    let (_, source_top) = app.offer_rows(app.column_width()).expect("source rows");
    assert_eq!(source_top, 0);
    app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.offer_rows(app.column_width()).expect("source rows").1,
        source_top + 3,
        "one wheel step shifts the source three lines"
    );
}

/// An app on home in `/w` at 160x40, attached, with a delegates panel.
fn panel_app() -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: vec!["delegates".to_owned()],
        ..Default::default()
    });
    app.attach(contract::SessionId(S_A.to_owned()));
    app.set_size(160, 40);
    app
}

/// Folds Fiber delegates `j_1` to `j_<n>`.
fn fold_delegates(app: &mut App, n: usize) {
    for k in 1..=n {
        let job = format!("j_{k}");
        app.on_line(session_line(
            S_A,
            "job_started",
            json!({"job_id": job, "description": format!("task {k}"), "output_path": "/tmp/out"}),
            Some(&format!("a_{k}")),
        ));
        app.on_line(session_line(
            S_A,
            "delegate_started",
            json!({"job_id": job, "delegate_session_id": format!("s_{job:0>16}"),
                "harness": "fiber", "model": "test/model", "workspace": "/w"}),
            Some(&format!("a_{k}")),
        ));
    }
}

/// A wheel over the panel with the offer also open keeps the panel's
/// existing behavior: the panel scrolls while the offer cursor and the
/// conversation stay (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_over_panel_with_offer_open_keeps_the_panel() {
    let mut app = panel_app();
    fold_delegates(&mut app, 5);
    // Scroll first, while the wheel still reaches the conversation: the
    // offer covers it once its rows draw. Column 40 clears the rail.
    app.on_wheel(&wheel(40, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    offered_lines(&mut app);
    assert!(app.offer_open());
    let top = app.top();
    let cursor = cursor_text(&app);
    let panel_x = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .map(|panel| panel.x.saturating_add(4))
        .expect("the panel draws");
    app.on_wheel(&wheel(panel_x, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(cursor_text(&app), cursor, "the wheel left the offer");
    assert!(
        app.panel_state().delegate_scroll() > 0,
        "the panel scrolled instead"
    );
}
