//! Tests for the repository offer's state.

use contract::events::OfferDecision;
use contract::{Envelope, SessionId};
use serde_json::{Value, json};

use super::{Offer, OfferKey, Spot, TUI_FILES};
use crate::keys::{Edit, Key};

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
    // Three items of four rows each, under three header rows, are taller
    // than five rows.
    let mut offer = holding(3);
    for _ in 0..3 {
        press(&mut offer, &Key::Down, 5);
    }
    let send = texts(&offer, 80)
        .iter()
        .position(|row| row.contains("Send"))
        .unwrap_or(0);
    assert_eq!(send, 15);
    assert_eq!(top(&offer), 11);
}

#[test]
fn moving_up_scrolls_back() {
    let mut offer = holding(3);
    for _ in 0..3 {
        press(&mut offer, &Key::Down, 5);
    }
    press(&mut offer, &Key::Up, 5);
    // The third item's header is row 11, still shown.
    assert_eq!(top(&offer), 11);
    press(&mut offer, &Key::Up, 5);
    assert_eq!(top(&offer), 7);
    press(&mut offer, &Key::Up, 5);
    assert_eq!(top(&offer), 3);
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
            "Repository code · 2 items              ✕",
            TUI_FILES,
            "",
            "› extension lint · 1.2.0 · required",
            "    [approve]  skip   never",
            "    extension lint",
            "    runs: node",
            "    -old",
            "    +new",
            "",
            "  hook fmt",
            "     approve  [skip]  never",
            "    hook fmt",
            "",
            "  Send · 1 approve, 1 skip, 0 never",
        ]
        .map(str::to_owned)
    );
}

#[test]
fn the_title_fills_the_header_before_the_close_target() {
    let offer = holding(1);
    let title = "Repository code · 1 item";
    let title_width = u16::try_from(crate::format::width(title)).expect("short title fits u16");

    let Some((rows, _)) = offer.rows(title_width + 1) else {
        panic!("open");
    };
    let header = rows.first().expect("header row");
    assert_eq!(header.line.to_string(), format!("{title}✕"));
    assert_eq!(header.spots, [(title_width, title_width + 1, Spot::Close)]);

    let Some((rows, _)) = offer.rows(title_width) else {
        panic!("open");
    };
    let header = rows.first().expect("header row");
    assert_eq!(
        header.line.to_string(),
        format!("{}✕", title.strip_suffix('m').expect("title ends in m"))
    );
    assert_eq!(header.spots, [(title_width - 1, title_width, Spot::Close)]);
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
    // " approve " starts at 4 and is cut at 12; "[skip]" starts at 14.
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
    assert!(header.ends_with('✕'), "{header}");
    assert_eq!(crate::format::width(&header), 12);
}
