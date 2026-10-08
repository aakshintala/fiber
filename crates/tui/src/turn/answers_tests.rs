//! Tests for what a question form leaves in its card: the "you answered"
//! rule and the ledger row's ending, driven through the app and the fold
//! as the hub's lines arrive.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::app::App;
use crate::keys::Key;
use crate::link::Line;
use crate::turn::{Fold, Turn, fold_line};
use crate::window::Pages;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One line of `kind` at `ts` milliseconds.
fn envelope(kind: &str, action: Option<&str>, ts: u64, payload: Value) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

fn started(ts: u64) -> contract::Envelope {
    envelope(
        "turn_started",
        None,
        ts,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "pick"}]}]}),
    )
}

/// `ask_user` called as `action` with two questions.
fn asked(action: &str, ts: u64) -> contract::Envelope {
    envelope(
        "tool_call_requested",
        Some(action),
        ts,
        json!({"name": "ask_user", "arguments":
            {"questions": [{"header": "Base"}, {"header": "Name"}]}}),
    )
}

/// The two questions the form asks.
fn fields() -> Value {
    json!([
        {"header": "Base", "question": "Which branch?", "multiSelect": true,
         "options": [{"label": "main (Recommended)", "description": "the default"},
                     {"label": "dev"}]},
        {"header": "Name", "question": "What name?"}
    ])
}

/// `interaction_requested` for a form of `fields` raised by `actions`.
fn form(request: &str, actions: &[&str], fields: Value) -> contract::Envelope {
    envelope(
        "interaction_requested",
        None,
        0,
        json!({"request_id": request, "kind": "form", "action_ids": actions, "fields": fields}),
    )
}

/// `interaction_resolved` for `request` by `by`, with `answer`'s keys.
fn resolved(request: &str, by: &str, answer: Value) -> contract::Envelope {
    let mut payload = json!({"request_id": request, "by": by});
    if let (Some(payload), Some(answer)) = (payload.as_object_mut(), answer.as_object()) {
        payload.extend(answer.clone());
    }
    envelope("interaction_resolved", None, 0, payload)
}

/// The answers the person gave to [`fields`].
fn answers() -> Value {
    json!({"answers": [{"labels": ["main (Recommended)", "dev"], "text": "and tags"},
        {"skipped": true}], "note": "by friday"})
}

fn declined() -> Value {
    json!({"declined": true})
}

fn completed(action: &str, status: &str, ts: u64) -> contract::Envelope {
    envelope(
        "tool_call_completed",
        Some(action),
        ts,
        json!({"status": status, "content": [{"type": "text", "text": "ok"}]}),
    )
}

/// An app attached to [`SESSION`], 60 columns wide, with every ledger
/// open.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.on_key(Key::CtrlO, fakes::clock::FakeClock::new().now());
    app
}

fn feed(app: &mut App, lines: impl IntoIterator<Item = contract::Envelope>) {
    for line in lines {
        app.on_line(Line::Session(line));
    }
}

fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

/// The rows of the "you answered" rules, each rule from its first row.
fn rules(app: &App) -> Vec<Vec<String>> {
    let texts = texts(app);
    texts
        .iter()
        .enumerate()
        .filter(|(_, text)| *text == "you answered")
        .map(|(at, _)| {
            texts
                .iter()
                .skip(at)
                .take_while(|text| !text.starts_with('▣'))
                .cloned()
                .collect()
        })
        .collect()
}

/// The ledger row of the `ask_user` call.
fn ledger(app: &App) -> String {
    texts(app)
        .into_iter()
        .find(|text| !text.starts_with('•') && text.contains(" ask_user "))
        .unwrap_or_default()
}

/// A turn that asked [`fields`] from `a_1`.
fn asking() -> Vec<contract::Envelope> {
    vec![
        started(0),
        asked("a_1", 0),
        form("r_4f", &["a_1"], fields()),
    ]
}

#[test]
fn an_answered_form_adds_the_rule_and_reads_answered() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", answers()),
            completed("a_1", "completed", 4000),
        ],
    );
    assert_eq!(
        rules(&app),
        vec![vec![
            "you answered",
            "Base: main (Recommended), dev, \"and tags\"",
            "Name: skipped",
            "note: \"by friday\"",
        ]]
    );
    assert!(ledger(&app).ends_with(" · answered"), "{}", ledger(&app));
}

#[test]
fn a_declined_form_adds_no_rule_and_reads_declined() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", declined()),
            completed("a_1", "completed", 0),
        ],
    );
    assert!(rules(&app).is_empty());
    assert!(ledger(&app).ends_with(" · declined"), "{}", ledger(&app));
}

#[test]
fn a_decline_the_cancel_overtook_still_reads_declined() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", declined()),
            completed("a_1", "cancelled", 0),
        ],
    );
    assert!(ledger(&app).ends_with(" · declined"), "{}", ledger(&app));
    assert!(!ledger(&app).contains("cancelled"));
}

#[test]
fn a_turn_cancel_reads_cancelled() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "fiber", declined()),
            completed("a_1", "cancelled", 0),
        ],
    );
    assert!(rules(&app).is_empty());
    assert!(ledger(&app).ends_with(" · cancelled"), "{}", ledger(&app));
}

#[test]
fn a_close_reads_as_a_completed_row() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "fiber", declined()),
            completed("a_1", "completed", 0),
        ],
    );
    let row = ledger(&app);
    assert!(rules(&app).is_empty());
    assert!(row.ends_with("\"Name\"}]}"), "{row}");
}

#[test]
fn a_pending_form_reads_running() {
    let mut app = app();
    feed(&mut app, asking());
    assert!(ledger(&app).ends_with(" · running"), "{}", ledger(&app));
    assert!(rules(&app).is_empty());
}

#[test]
fn a_re_raised_form_draws_one_rule() {
    let mut app = app();
    feed(&mut app, asking());
    let exited = json!({"exit_code": 0, "suspended_on": "r_4f", "usage": {"tokens":
        {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
        "cost": 0, "subscription_cost": 0}});
    let mut again = form("r_4f", &["a_1"], fields());
    again.payload.insert("resumes".to_owned(), json!(true));
    feed(
        &mut app,
        [
            envelope("fiber_exited", None, 0, exited),
            envelope(
                "fiber_started",
                None,
                0,
                json!({"version": "0.0.1", "resumed": true}),
            ),
            again,
            resolved("r_4f", "person", answers()),
            completed("a_1", "completed", 0),
        ],
    );
    assert_eq!(rules(&app).len(), 1);
    assert!(ledger(&app).ends_with(" · answered"), "{}", ledger(&app));
}

/// Folds `lines` into fresh turns, returning them with each fold's result.
fn folded(lines: &[contract::Envelope]) -> (Vec<Turn>, Vec<bool>) {
    let mut turns = Vec::new();
    let mut fold = Fold::default();
    let changed = lines
        .iter()
        .map(|line| fold_line(&mut turns, &mut fold, line))
        .collect();
    (turns, changed)
}

#[test]
fn a_confirm_interaction_naming_a_call_changes_nothing() {
    let confirm = envelope(
        "interaction_requested",
        None,
        0,
        json!({"request_id": "r_c", "kind": "confirm", "prompt": "ok?", "action_ids": ["a_1"]}),
    );
    let lines = vec![
        started(0),
        asked("a_1", 0),
        confirm,
        resolved("r_c", "person", declined()),
    ];
    let (turns, changed) = folded(&lines);
    assert_eq!(changed, vec![true, true, false, false]);
    assert!(turns.iter().all(|turn| turn.entries.len() == 1));
    let mut app = app();
    feed(&mut app, lines);
    assert!(ledger(&app).ends_with(" · running"), "{}", ledger(&app));
}

#[test]
fn a_form_naming_no_known_call_changes_nothing() {
    let (turns, changed) = folded(&[
        started(0),
        asked("a_1", 0),
        form("r_4f", &["a_9"], fields()),
        resolved("r_4f", "person", answers()),
    ]);
    assert_eq!(changed, vec![true, true, false, false]);
    assert!(turns.iter().all(|turn| turn.entries.len() == 1));
}

#[test]
fn a_form_from_an_extension_names_no_call_and_changes_nothing() {
    let mut line = form("r_4f", &[], fields());
    line.payload.remove("action_ids");
    line.payload.insert("extension".to_owned(), json!("x"));
    let (_, changed) = folded(&[
        started(0),
        asked("a_1", 0),
        line,
        resolved("r_4f", "person", answers()),
    ]);
    assert_eq!(changed, vec![true, true, false, false]);
}

#[test]
fn a_duplicate_resolution_draws_one_rule() {
    let mut lines = asking();
    lines.push(resolved("r_4f", "person", answers()));
    lines.push(resolved("r_4f", "person", answers()));
    lines.push(resolved("r_4f", "person", declined()));
    let (_, changed) = folded(&lines);
    assert_eq!(changed, vec![true, true, true, true, false, false]);
    let mut app = app();
    feed(&mut app, lines);
    assert_eq!(rules(&app).len(), 1);
    assert!(ledger(&app).ends_with(" · answered"), "{}", ledger(&app));
}

#[test]
fn a_person_answer_of_another_kind_changes_nothing() {
    let mut lines = asking();
    lines.push(resolved("r_4f", "person", json!({"labels": ["dev"]})));
    let (_, changed) = folded(&lines);
    assert_eq!(changed.last(), Some(&false));
    let mut app = app();
    feed(&mut app, lines);
    assert!(rules(&app).is_empty());
    assert!(ledger(&app).ends_with(" · running"), "{}", ledger(&app));
    // It is still waiting: the person's answer that follows counts.
    feed(&mut app, [resolved("r_4f", "person", answers())]);
    assert!(ledger(&app).ends_with(" · answered"), "{}", ledger(&app));
}

#[test]
fn a_new_request_id_on_the_same_call_replaces_asked() {
    let lines = vec![
        started(0),
        asked("a_1", 0),
        form(
            "r_1",
            &["a_1"],
            json!([{"header": "Base", "question": "?"}]),
        ),
        form(
            "r_2",
            &["a_1"],
            json!([{"header": "Name", "question": "?"}]),
        ),
        resolved("r_1", "person", json!({"answers": [{"skipped": true}]})),
        resolved(
            "r_2",
            "person",
            json!({"answers": [{"labels": [], "text": "fiber-cli"}]}),
        ),
        resolved("r_2", "person", json!({"answers": [{"skipped": true}]})),
    ];
    let (_, changed) = folded(&lines);
    assert_eq!(changed, vec![true, true, true, true, false, true, false]);
    let mut app = app();
    feed(&mut app, lines);
    assert_eq!(
        rules(&app),
        vec![vec!["you answered", "Name: \"fiber-cli\""]]
    );
    assert!(ledger(&app).ends_with(" · answered"), "{}", ledger(&app));
}

#[test]
fn answers_shorter_than_fields_draw_only_answered_rows() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [resolved(
            "r_4f",
            "person",
            json!({"answers": [{"labels": ["dev"]}]}),
        )],
    );
    assert_eq!(rules(&app), vec![vec!["you answered", "Base: dev"]]);
}

#[test]
fn answers_longer_than_fields_drop_the_extra() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [resolved(
            "r_4f",
            "person",
            json!({"answers": [{"skipped": true}, {"skipped": true}, {"labels": ["x"]}]}),
        )],
    );
    assert_eq!(
        rules(&app),
        vec![vec!["you answered", "Base: skipped", "Name: skipped"]]
    );
}

#[test]
fn the_rule_escapes_newlines_and_quotes() {
    let mut app = app();
    feed(
        &mut app,
        [
            started(0),
            asked("a_1", 0),
            form(
                "r_4f",
                &["a_1"],
                json!([{"header": "Pick\nnow", "question": "?",
                    "options": [{"label": "a\tb \"c\""}, {"label": "d"}]}]),
            ),
            resolved(
                "r_4f",
                "person",
                json!({"answers": [{"labels": ["a\tb \"c\""], "text": "x\ny"}]}),
            ),
        ],
    );
    assert_eq!(
        rules(&app),
        vec![vec![
            "you answered",
            "Pick\\nnow: a\\tb \\\"c\\\", \"x\\ny\"",
        ]]
    );
}

#[test]
fn the_rule_sits_where_the_answer_landed_and_the_group_stays_open() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", answers()),
            completed("a_1", "completed", 0),
            envelope(
                "tool_call_requested",
                Some("a_2"),
                0,
                json!({"name": "read", "arguments": {"path": "a.rs"}}),
            ),
            completed("a_2", "completed", 0),
        ],
    );
    let texts = texts(&app);
    let read = texts.iter().position(|text| text.ends_with("read a.rs"));
    let rule = texts.iter().position(|text| text == "you answered");
    assert!(read.is_some() && read < rule, "{texts:#?}");
    let groups = texts.iter().filter(|text| text.starts_with("• ")).count();
    assert_eq!(groups, 1, "{texts:#?}");
}

/// Durable lines `lines`, numbered from 1.
fn numbered(lines: Vec<contract::Envelope>) -> Vec<contract::Envelope> {
    lines
        .into_iter()
        .zip(1..)
        .map(|(mut line, seq)| {
            line.seq = Some(contract::Seq(seq));
            line
        })
        .collect()
}

#[test]
fn a_page_loaded_from_history_draws_the_same_card() {
    // The answered turn, then enough lines that the next turn starts a
    // page and the first can be dropped.
    let mut lines = asking();
    lines.push(resolved("r_4f", "person", answers()));
    lines.push(completed("a_1", "completed", 0));
    lines.extend(
        (0..crate::pages::PAGE_LINES)
            .map(|_| envelope("assistant_message_started", Some("a_m"), 0, json!({}))),
    );
    lines.push(envelope(
        "turn_completed",
        None,
        0,
        json!({"outcome": "completed"}),
    ));
    lines.push(started(0));
    let lines = numbered(lines);
    let mut pages = Pages::new(60);
    for line in &lines {
        pages.apply(line);
    }
    let live = pages.rows();
    assert!(
        live.iter()
            .any(|(line, _)| line.to_string() == "you answered"),
        "the rule is drawn live"
    );
    assert_eq!(pages.page_count(), 2);
    // The window reaches a row above its top: start it a row into page 1.
    pages.trim(pages.index().start(1).saturating_add(1), 1);
    assert!(pages.part(0).is_none(), "the first page was dropped");
    let first = pages.index().pages().first().cloned().unwrap_or_default();
    let history: Vec<contract::Envelope> = lines
        .iter()
        .filter(|line| {
            line.seq
                .is_some_and(|seq| (first.first_seq..=first.last_seq).contains(&seq))
        })
        .cloned()
        .collect();
    pages.load(&history);
    assert!(pages.part(0).is_some(), "the first page loaded again");
    assert_eq!(pages.rows(), live);
}

#[test]
fn fold_reports_a_change() {
    let mut again = form("r_4f", &["a_1"], fields());
    again.payload.insert("resumes".to_owned(), json!(true));
    let lines = vec![
        started(0),
        asked("a_1", 0),
        form("r_4f", &["a_1"], fields()),
        again,
        resolved("r_4f", "person", answers()),
    ];
    let mut pages = Pages::new(60);
    let changed: Vec<bool> = numbered(lines)
        .iter()
        .map(|line| pages.apply(line).changed)
        .collect();
    assert_eq!(changed, vec![true, true, true, false, true]);
}

/// The answered card's screen at `width` by 16.
fn answered_screen(width: u16) -> String {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, 16);
    app.attach(contract::SessionId(SESSION.to_owned()));
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", answers()),
            completed("a_1", "completed", 4000),
            envelope(
                "text_completed",
                Some("a_m"),
                4000,
                json!({"text": "Thanks."}),
            ),
        ],
    );
    let area = Rect::new(0, 0, width, 16);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn card_with_you_answered() {
    insta::assert_snapshot!("card_with_you_answered", answered_screen(60));
}

#[test]
fn card_with_you_answered_at_30() {
    insta::assert_snapshot!("card_with_you_answered_at_30", answered_screen(30));
}
