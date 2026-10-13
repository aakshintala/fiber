//! Tests for the repository offer's state.

use contract::events::OfferDecision;
use contract::{Envelope, SessionId};
use ratatui::style::{Modifier, Style};
use serde_json::{Value, json};

use super::{HINT, Offer, OfferKey, Spot, TUI_FILES};
use crate::keys::{Edit, Key};
use crate::markdown::style;
use crate::theme::Role;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One envelope of `kind` from [`SESSION`].
fn envelope(kind: &str, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// An offer `id` of `count` MCP servers named `a`, `b`, ...
fn offered(id: &str, count: u8) -> Envelope {
    let items: Vec<Value> = (0..count)
        .map(|at| {
            let name = char::from(b'a'.saturating_add(at)).to_string();
            json!({"kind": "mcp_server", "name": name, "hash": "h", "required": false,
                "summary": format!("MCP server: {name}")})
        })
        .collect();
    envelope(
        "repository_code_offered",
        json!({"request_id": id, "items": items}),
    )
}

/// A resolution of offer `id`.
fn resolved(id: &str) -> Envelope {
    envelope(
        "repository_code_resolved",
        json!({"request_id": id, "decisions": ["skip"]}),
    )
}

/// An offer holding `count` items under `r_1`.
fn holding(count: u8) -> Offer {
    let mut offer = Offer::default();
    offer.fold(&offered("r_1", count));
    offer
}

/// An offer of `items` under `r_1`.
fn with_items(items: Value) -> Offer {
    let mut offer = Offer::default();
    offer.fold(&envelope(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": items}),
    ));
    offer
}

/// One item of `kind` named `name`, with `version` and `summary`.
fn item(kind: &str, name: &str, version: Option<&str>, summary: &str) -> Value {
    let mut item = json!({"kind": kind, "name": name, "hash": "h",
        "required": false, "summary": summary});
    if let Some(version) = version {
        item["version"] = json!(version);
    }
    item
}

/// Each row's spans at `width`: every span's text and style.
fn spans(offer: &Offer, width: u16) -> Vec<Vec<(String, Style)>> {
    offer
        .rows(width)
        .map(|(rows, _)| {
            rows.iter()
                .map(|row| {
                    row.line
                        .spans
                        .iter()
                        .map(|span| (span.content.to_string(), span.style))
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The dim style the install lines, the footer and the hunk `@@` lines
/// use.
fn dim() -> Style {
    style(Role::Muted)
}

/// Bold on the default colour, as the top row's name and a chosen
/// answer's label draw.
fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// The cursor row's style.
fn secondary() -> Style {
    style(Role::Secondary)
}

/// Presses `key` at 80 columns and `height` rows.
fn press(offer: &mut Offer, key: &Key, height: usize) -> Option<OfferKey> {
    offer.on_key(key, 80, height)
}

/// The reply line answering the offer, parsed.
fn reply(offer: &mut Offer) -> Value {
    let line = offer
        .answer("c_1", &SessionId(SESSION.to_owned()))
        .unwrap_or_default();
    serde_json::from_str(&line).unwrap_or_default()
}

/// The decisions the reply carries.
fn decisions(offer: &mut Offer) -> Value {
    reply(offer)["args"]["decisions"].clone()
}

/// The rows' text at `width`, trailing spaces trimmed.
fn texts(offer: &Offer, width: u16) -> Vec<String> {
    offer
        .rows(width)
        .map(|(rows, _)| {
            rows.iter()
                .map(|row| row.line.to_string().trim_end().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The top row.
fn top(offer: &Offer) -> usize {
    offer.rows(80).map_or(0, |(_, top)| top)
}

#[test]
fn a_new_offer_is_open() {
    let offer = holding(1);
    assert!(offer.open());
    assert!(!offer.aside());
    assert!(!Offer::default().open());
}

#[test]
fn a_payload_that_does_not_parse_is_skipped() {
    let mut offer = Offer::default();
    offer.fold(&envelope("repository_code_offered", json!({"items": 3})));
    assert!(!offer.open());
}

#[test]
fn a_repeated_offer_keeps_the_choices() {
    let mut offer = holding(2);
    offer.on_edit(&Edit::Left);
    offer.fold(&offered("r_1", 2));
    assert_eq!(decisions(&mut offer), json!(["approve", "skip"]));
}

#[test]
fn a_new_offer_replaces_the_held_one() {
    let mut offer = holding(2);
    offer.on_edit(&Edit::Left);
    offer.fold(&offered("r_2", 1));
    let line = reply(&mut offer);
    assert_eq!(line["command"], "reply");
    assert_eq!(line["session_id"], SESSION);
    assert_eq!(
        line["args"],
        json!({"request_id": "r_2", "decisions": ["skip"]})
    );
}

#[test]
fn its_resolution_closes_the_offer() {
    let mut offer = holding(1);
    offer.fold(&resolved("r_1"));
    assert!(!offer.open());
    assert!(offer.rows(80).is_none());
}

#[test]
fn another_resolution_keeps_it() {
    let mut offer = holding(1);
    offer.fold(&resolved("r_9"));
    assert!(offer.open());
}

#[test]
fn put_aside_closes_it() {
    let mut offer = holding(1);
    offer.put_aside();
    assert!(!offer.open());
    assert!(offer.aside());
    assert!(offer.rows(80).is_none());
    assert_eq!(press(&mut offer, &Key::Down, 20), None);
    assert!(!offer.on_edit(&Edit::Left));
}

#[test]
fn answered_closes_it() {
    let mut offer = holding(1);
    assert!(
        offer
            .answer("c_1", &SessionId(SESSION.to_owned()))
            .is_some()
    );
    assert!(!offer.open());
    assert!(
        offer
            .answer("c_2", &SessionId(SESSION.to_owned()))
            .is_none()
    );
}

#[test]
fn the_badge_skips_it_while_answered() {
    let mut offer = holding(1);
    offer.answer("c_1", &SessionId(SESSION.to_owned()));
    assert!(!offer.aside());
}

#[test]
fn every_item_starts_at_skip() {
    let mut offer = holding(3);
    assert_eq!(decisions(&mut offer), json!(["skip", "skip", "skip"]));
}

#[test]
fn an_offer_of_no_items_sends_an_empty_list() {
    let mut offer = holding(0);
    assert_eq!(press(&mut offer, &Key::Enter, 20), Some(OfferKey::Send));
    assert_eq!(decisions(&mut offer), json!([]));
}

#[test]
fn down_stops_at_send() {
    let mut offer = holding(2);
    for _ in 0..5 {
        press(&mut offer, &Key::Down, 40);
    }
    assert_eq!(press(&mut offer, &Key::Enter, 40), Some(OfferKey::Send));
    press(&mut offer, &Key::Up, 40);
    assert_eq!(press(&mut offer, &Key::Enter, 40), Some(OfferKey::Handled));
}

#[test]
fn up_stops_at_the_first_item() {
    let mut offer = holding(2);
    press(&mut offer, &Key::Up, 40);
    offer.on_edit(&Edit::Right);
    assert_eq!(decisions(&mut offer), json!(["never", "skip"]));
}

#[test]
fn enter_on_an_item_moves_down() {
    let mut offer = holding(2);
    assert_eq!(press(&mut offer, &Key::Enter, 40), Some(OfferKey::Handled));
    offer.on_edit(&Edit::Left);
    assert_eq!(decisions(&mut offer), json!(["skip", "approve"]));
}

#[test]
fn enter_on_send_sends() {
    let mut offer = holding(1);
    assert_eq!(press(&mut offer, &Key::Enter, 40), Some(OfferKey::Handled));
    assert_eq!(press(&mut offer, &Key::Enter, 40), Some(OfferKey::Send));
}

#[test]
fn left_stops_at_approve() {
    let mut offer = holding(1);
    assert!(offer.on_edit(&Edit::Left));
    assert!(offer.on_edit(&Edit::Left));
    assert_eq!(decisions(&mut offer), json!(["approve"]));
}

#[test]
fn right_stops_at_never() {
    let mut offer = holding(1);
    offer.on_edit(&Edit::Right);
    offer.on_edit(&Edit::Right);
    assert_eq!(decisions(&mut offer), json!(["never"]));
}

#[test]
fn left_right_on_send_do_nothing() {
    let mut offer = holding(1);
    press(&mut offer, &Key::Down, 40);
    assert!(offer.on_edit(&Edit::Left));
    assert!(offer.on_edit(&Edit::Right));
    assert_eq!(decisions(&mut offer), json!(["skip"]));
}

#[test]
fn other_edits_are_swallowed() {
    let mut offer = holding(1);
    assert!(offer.on_edit(&Edit::Paste("x".to_owned())));
    assert!(offer.on_edit(&Edit::Delete));
    assert_eq!(decisions(&mut offer), json!(["skip"]));
}

#[test]
fn esc_f1_alt_a_and_ctrl_c_pass_and_the_rest_is_swallowed() {
    let mut offer = holding(1);
    for key in [Key::Esc, Key::F1, Key::AltA, Key::CtrlC] {
        assert_eq!(press(&mut offer, &key, 20), None, "{key:?}");
    }
    for key in [Key::Char('x'), Key::Tab, Key::BackTab, Key::End, Key::CtrlR] {
        assert_eq!(
            press(&mut offer, &key, 20),
            Some(OfferKey::Handled),
            "{key:?}"
        );
    }
    assert!(offer.open());
}

#[test]
fn moving_down_scrolls_send_into_view() {
    // Three items of four rows each, under two header rows and over four
    // footer rows, are taller than five rows.
    let mut offer = holding(3);
    for _ in 0..3 {
        press(&mut offer, &Key::Down, 5);
    }
    let send = texts(&offer, 80)
        .iter()
        .position(|row| row.contains("Send ·"))
        .unwrap_or(0);
    assert_eq!(send, 17);
    assert_eq!(top(&offer), 13);
}

#[test]
fn moving_up_scrolls_back() {
    let mut offer = holding(3);
    for _ in 0..3 {
        press(&mut offer, &Key::Down, 5);
    }
    press(&mut offer, &Key::Up, 5);
    // The third item's header is row 10, still shown.
    assert_eq!(top(&offer), 10);
    press(&mut offer, &Key::Up, 5);
    assert_eq!(top(&offer), 6);
    press(&mut offer, &Key::Up, 5);
    assert_eq!(top(&offer), 2);
}

#[test]
fn moving_at_the_top_row_with_zero_height_moves_top_past_it() {
    let mut offer = holding(1);
    for _ in 0..3 {
        press(&mut offer, &Key::PageDown, 0);
    }
    assert_eq!(top(&offer), 3);

    press(&mut offer, &Key::Up, 0);
    assert_eq!(top(&offer), 2);
}

#[test]
fn moving_above_top_with_zero_height_moves_top_to_the_cursor() {
    let mut offer = holding(1);
    for _ in 0..4 {
        press(&mut offer, &Key::PageDown, 0);
    }
    assert_eq!(top(&offer), 4);

    press(&mut offer, &Key::Up, 0);
    assert_eq!(top(&offer), 2);
}

#[test]
fn page_down_stops_at_the_end() {
    let mut offer = holding(3);
    let total = texts(&offer, 80).len();
    press(&mut offer, &Key::PageDown, 5);
    assert_eq!(top(&offer), 4);
    for _ in 0..10 {
        press(&mut offer, &Key::PageDown, 5);
    }
    assert_eq!(top(&offer), total.saturating_sub(5));
}

#[test]
fn page_up_stops_at_the_top() {
    let mut offer = holding(3);
    press(&mut offer, &Key::PageDown, 5);
    press(&mut offer, &Key::PageDown, 5);
    press(&mut offer, &Key::PageUp, 5);
    assert_eq!(top(&offer), 4);
    for _ in 0..3 {
        press(&mut offer, &Key::PageUp, 5);
    }
    assert_eq!(top(&offer), 0);
}

#[test]
fn restore_reopens_it_with_its_choices() {
    let mut offer = holding(1);
    offer.on_edit(&Edit::Right);
    offer.answer("c_1", &SessionId(SESSION.to_owned()));
    offer.restore("c_1");
    assert!(offer.open());
    assert_eq!(decisions(&mut offer), json!(["never"]));
}

#[test]
fn restore_ignores_another_id() {
    let mut offer = holding(1);
    offer.answer("c_1", &SessionId(SESSION.to_owned()));
    offer.restore("c_2");
    assert!(!offer.open());
}

#[test]
fn restore_while_aside_does_nothing() {
    let mut offer = holding(1);
    offer.put_aside();
    offer.restore("c_1");
    assert!(offer.aside());
    assert!(!offer.open());
}

#[test]
fn reopen_opens_it_after_put_aside() {
    let mut offer = holding(1);
    offer.put_aside();
    assert!(offer.reopen());
    assert!(offer.open());
    assert!(!offer.reopen());
}

#[test]
fn reopen_while_answered_does_nothing() {
    let mut offer = holding(1);
    offer.answer("c_1", &SessionId(SESSION.to_owned()));
    assert!(!offer.reopen());
    assert!(!offer.open());
    assert!(!Offer::default().reopen());
}

#[test]
fn a_chip_click_sets_the_decision_and_moves_the_cursor() {
    let mut offer = holding(2);
    let spot = Spot::Choice {
        item: 1,
        decision: OfferDecision::Never,
    };
    assert_eq!(offer.click(spot), OfferKey::Handled);
    // ← now steps the second item, from never to skip.
    offer.on_edit(&Edit::Left);
    assert_eq!(decisions(&mut offer), json!(["skip", "skip"]));
}

#[test]
fn send_and_close_clicks() {
    let mut offer = holding(1);
    assert_eq!(offer.click(Spot::Send), OfferKey::Send);
    assert_eq!(offer.click(Spot::Close), OfferKey::Handled);
    assert!(offer.aside());
    // Closed, a click does nothing.
    assert_eq!(offer.click(Spot::Send), OfferKey::Handled);
}

#[test]
fn the_rows_show_each_item_and_the_tui_files_line() {
    let mut offer = Offer::default();
    offer.fold(&envelope(
        "repository_code_offered",
        json!({"request_id": "r_1", "items": [
            {"kind": "extension", "name": "lint", "hash": "h", "required": true,
             "summary": "extension lint\nruns: node", "version": "1.2.0",
             "diff": "-old\n+new"},
            {"kind": "hook", "name": "fmt", "hash": "h", "required": false, "summary": "hook fmt"},
        ]}),
    ));
    offer.on_edit(&Edit::Left);
    assert_eq!(
        texts(&offer, 40),
        [
            " Offer  repository code      Esc returns",
            "",
            "▸ Extension lint · 1.2.0",
            "  extension lint",
            "  runs: node",
            "  the declaration changed",
            "    -old",
            "    +new",
            "    [approve]  skip   never",
            "",
            "  Hook fmt",
            "     approve  [skip]  never",
            "  hook fmt",
            "",
            TUI_FILES,
            "",
            HINT,
            "  Send · 1 approve, 1 skip, 0 never",
        ]
        .map(str::to_owned)
    );
}

#[test]
fn the_top_row_names_the_offer_with_its_command_dim() {
    let offer = holding(1);
    let pad = " ".repeat(80 - 6 - 17 - 11);
    assert_eq!(
        spans(&offer, 80)[0],
        [
            (" ".to_owned(), Style::new()),
            ("Offer".to_owned(), bold()),
            ("  repository code".to_owned(), dim()),
            (pad, Style::new()),
            ("Esc returns".to_owned(), dim()),
        ]
    );
    // A click anywhere on the top row puts the offer aside.
    let spots = offer
        .rows(80)
        .map(|(rows, _)| rows[0].spots.clone())
        .unwrap_or_default();
    assert_eq!(spots, [(0, 80, Spot::Close)]);
}

#[test]
fn a_narrow_top_row_drops_esc_returns_first() {
    let offer = holding(1);
    // 6 + 17 + 1 + 11 is 35 columns: Esc with one space before it.
    assert_eq!(texts(&offer, 35)[0], " Offer  repository code Esc returns");
    assert_eq!(texts(&offer, 34)[0], " Offer  repository code");
}

#[test]
fn a_tiny_top_row_truncates_the_command_then_the_name() {
    let offer = holding(1);
    assert_eq!(texts(&offer, 20)[0], " Offer  repository c");
    assert_eq!(texts(&offer, 4)[0], " Off");
}

#[test]
fn a_target_starting_at_the_width_is_dropped() {
    let width = 10;
    let row = super::clipped(
        ratatui::text::Line::raw("abcdefghijk"),
        vec![
            (width - 1, width + 2, Spot::Close),
            (width, width + 1, Spot::Send),
        ],
        width,
    );

    assert_eq!(row.spots, [(width - 1, width, Spot::Close)]);
}

#[test]
fn a_narrow_view_drops_the_clipped_targets() {
    let offer = holding(1);
    let Some((rows, _)) = offer.rows(12) else {
        panic!("open");
    };
    // "    [approve]  skip   never" is the old choices row: task 2
    // restyles it, and this spot moves with it.
    let choice = rows.get(4).map(|row| row.spots.clone()).unwrap_or_default();
    assert_eq!(
        choice,
        [(
            4,
            12,
            Spot::Choice {
                item: 0,
                decision: OfferDecision::Approve
            }
        )]
    );
    let header = rows
        .first()
        .map(|row| row.line.to_string())
        .unwrap_or_default();
    assert_eq!(header, " Offer  repo");
    assert_eq!(crate::format::width(&header), 12);
}

#[test]
fn the_chosen_item_marks_its_header_in_secondary() {
    let offer = holding(2);
    let rows = spans(&offer, 80);
    assert_eq!(
        rows[2],
        [
            ("▸ ".to_owned(), secondary()),
            ("MCP server a".to_owned(), secondary()),
        ]
    );
    assert_eq!(
        rows[6],
        [
            ("  ".to_owned(), Style::new()),
            ("MCP server b".to_owned(), Style::new()),
        ]
    );
}

#[test]
fn moving_down_moves_the_mark_and_the_secondary_row() {
    let mut offer = holding(2);
    press(&mut offer, &Key::Down, 40);
    let rows = spans(&offer, 80);
    assert_eq!(rows[2][0], ("  ".to_owned(), Style::new()));
    assert_eq!(rows[2][1].1, Style::new());
    assert_eq!(rows[6][0], ("▸ ".to_owned(), secondary()));
    assert_eq!(rows[6][1].1, secondary());
}

#[test]
fn an_extension_header_carries_its_version_verbatim() {
    let offer = with_items(json!([item(
        "extension",
        "notes",
        Some("v0.3"),
        "name: notes"
    )]));
    assert_eq!(texts(&offer, 80)[2], "▸ Extension notes · v0.3");
}

#[test]
fn an_extension_without_a_version_names_kind_and_name() {
    let offer = with_items(json!([item("extension", "lint", None, "name: lint")]));
    assert_eq!(texts(&offer, 80)[2], "▸ Extension lint");
}

#[test]
fn a_hook_header_carries_its_declaration_path() {
    let offer = with_items(json!([item(
        "hook",
        "tidy",
        None,
        "Hook: tidy\ndeclared in: .fiber/hooks.json\nruns: scripts/tidy.sh"
    )]));
    assert_eq!(
        texts(&offer, 80)[2],
        "▸ Hook tidy · declared in .fiber/hooks.json"
    );
}

#[test]
fn an_item_without_a_declaration_path_names_kind_and_name() {
    let offer = with_items(json!([item("mcp_server", "db", None, "MCP server: db")]));
    assert_eq!(texts(&offer, 80)[2], "▸ MCP server db");
}

#[test]
fn a_bare_declared_in_line_names_kind_and_name() {
    let offer = with_items(json!([item(
        "mcp_server",
        "db",
        None,
        "MCP server: db\ndeclared in:"
    )]));
    assert_eq!(texts(&offer, 80)[2], "▸ MCP server db");
}

#[test]
fn install_lines_are_dim_and_indented() {
    let offer = with_items(json!([item(
        "mcp_server",
        "db",
        None,
        "MCP server: db\ndeclared in: .fiber/mcp.json"
    )]));
    let rows = spans(&offer, 80);
    assert_eq!(rows[3], [("  MCP server: db".to_owned(), dim())]);
    assert_eq!(
        rows[4],
        [("  declared in: .fiber/mcp.json".to_owned(), dim())]
    );
}

#[test]
fn the_footer_is_the_tui_files_sentence_then_the_hint_both_dim() {
    let offer = holding(1);
    let rows = spans(&offer, 80);
    assert_eq!(rows[6], [(TUI_FILES.to_owned(), dim())]);
    assert_eq!(rows[7], [("".to_owned(), Style::new())]);
    assert_eq!(rows[8], [(HINT.to_owned(), dim())]);
}

#[test]
fn the_send_row_marks_the_cursor_in_secondary() {
    let mut offer = holding(1);
    let last = spans(&offer, 80).len().saturating_sub(1);
    assert_eq!(
        spans(&offer, 80)[last],
        [
            (" ".to_owned(), Style::new()),
            (
                " Send · 0 approve, 1 skip, 0 never".to_owned(),
                Style::new()
            ),
        ]
    );
    press(&mut offer, &Key::Down, 40);
    assert_eq!(
        spans(&offer, 80)[last],
        [
            ("▸".to_owned(), secondary()),
            (" Send · 0 approve, 1 skip, 0 never".to_owned(), secondary()),
        ]
    );
}
