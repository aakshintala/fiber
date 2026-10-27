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

/// The answered rule's row at 60 columns with the resolution at midnight
/// UTC: the time and the rule filling the card's inner 58 columns.
fn rule_row() -> String {
    format!(" you answered · 00:00 {}", "─".repeat(58 - 12 - 10))
}

/// The rows of the "you answered" rules, each rule from its first row.
/// The first row carries the resolution time and the rule.
/// A card edge row carries no text: the card's half-block edges.
fn is_edge(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|ch| ch == '▄' || ch == '▀')
}

fn rules(app: &App) -> Vec<Vec<String>> {
    let texts = texts(app);
    texts
        .iter()
        .enumerate()
        .filter(|(_, text)| text.trim_start().starts_with("you answered"))
        .map(|(at, _)| {
            texts
                .iter()
                .skip(at)
                .take_while(|text| !text.starts_with('▣') && !is_edge(text))
                .cloned()
                .collect()
        })
        .collect()
}

/// The ledger row of the `ask_user` call: its kind cell reads `ask`.
/// The step's first row carries its number ahead of the glyph, so the
/// glyph is searched, not first.
fn ledger(app: &App) -> String {
    texts(app)
        .into_iter()
        .find(|text| {
            let trimmed = text.trim_start();
            !trimmed.starts_with('●')
                && trimmed.contains(['○', '✓', '⊘', '✗', '?'])
                && trimmed.contains("ask")
        })
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
            rule_row(),
            " Base         main (Recommended), dev, \"and tags\"".to_owned(),
            " Name         skipped".to_owned(),
            " note \"by friday\"".to_owned(),
        ]]
    );
    assert!(ledger(&app).ends_with("answered"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("declined"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("declined"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("cancelled"), "{}", ledger(&app));
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
    assert!(row.ends_with("ok"), "{row}");
}

#[test]
fn a_pending_form_waits_on_you() {
    let mut app = app();
    feed(&mut app, asking());
    assert!(ledger(&app).ends_with("waiting on you"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("answered"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("waiting on you"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("answered"), "{}", ledger(&app));
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
    assert!(ledger(&app).ends_with("waiting on you"), "{}", ledger(&app));
    // It is still waiting: the person's answer that follows counts.
    feed(&mut app, [resolved("r_4f", "person", answers())]);
    assert!(ledger(&app).ends_with("answered"), "{}", ledger(&app));
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
        vec![vec![rule_row(), " Name         \"fiber-cli\"".to_owned()]]
    );
    assert!(ledger(&app).ends_with("answered"), "{}", ledger(&app));
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
    assert_eq!(
        rules(&app),
        vec![vec![rule_row(), " Base         dev".to_owned()]]
    );
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
        vec![vec![
            rule_row(),
            " Base         skipped".to_owned(),
            " Name         skipped".to_owned()
        ]]
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
            rule_row(),
            " Pick\\nnow    a\\tb \\\"c\\\", \"x\\ny\"".to_owned(),
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
    let read = texts.iter().position(|text| {
        let trimmed = text.trim_start();
        !trimmed.starts_with('●')
            && trimmed.contains(['○', '✓', '⊘', '✗', '?'])
            && trimmed.contains("read")
    });
    let rule = texts
        .iter()
        .position(|text| text.trim_start().starts_with("you answered"));
    assert!(read.is_some() && read < rule, "{texts:#?}");
    let groups = texts.iter().filter(|text| text.starts_with(" ● ")).count();
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
            .any(|(line, _)| line.to_string().contains("you answered")),
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
    pages.load(&contract::SessionId(SESSION.to_owned()), &history);
    assert!(pages.part(0).is_some(), "the first page loaded again");
    assert_eq!(pages.rows(), live);
}

#[test]
fn the_rule_fill_is_decoration_so_its_dashes_are_no_answer_text() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", answers()),
            completed("a_1", "completed", 4000),
        ],
    );
    let pages = app.pages();
    let rules: Vec<String> = (0..pages.page_count())
        .filter_map(|at| pages.page_text_open(at))
        .flat_map(|(rows, texts)| crate::logical::logical(&rows, &texts))
        .map(|line| line.text)
        .filter(|text| text.trim_start().starts_with("you answered"))
        .collect();
    assert_eq!(rules, vec!["you answered · 00:00".to_owned()]);
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
    app.set_zone(jiff::tz::TimeZone::UTC);
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

#[test]
fn the_group_line_counts_the_questions_asked() {
    let mut app = app();
    let ask = |action: &str, questions: Value| {
        envelope(
            "tool_call_requested",
            Some(action),
            0,
            json!({"name": "ask_user", "arguments": {"questions": questions}}),
        )
    };
    feed(
        &mut app,
        [
            started(0),
            ask("a_1", json!([{}, {}, {}])),
            completed("a_1", "completed", 0),
            ask("a_2", json!([{}])),
            completed("a_2", "completed", 0),
            ask("a_3", json!([])),
            completed("a_3", "completed", 0),
        ],
    );
    let texts = texts(&app);
    assert!(
        texts
            .iter()
            .any(|text| text.starts_with(" ● Asked 4 questions, called 1 tool")),
        "{texts:#?}"
    );
}

/// The card's styled rows for `answered` at `width` columns in `zone`.
fn drawn(
    answered: &super::Answered,
    width: u16,
    zone: &jiff::tz::TimeZone,
) -> Vec<ratatui::text::Line<'static>> {
    let mut out = crate::rows::Rows::default();
    answered.rows(width, zone, &mut out);
    let (rows, _) = out.into_parts();
    rows.into_iter().map(|(line, _)| line).collect()
}

/// Each span's text and style.
fn spans(line: &ratatui::text::Line<'_>) -> Vec<(String, ratatui::style::Style)> {
    line.spans
        .iter()
        .map(|span| (span.content.to_string(), span.style))
        .collect()
}

fn accent_bold() -> ratatui::style::Style {
    crate::theme::style(crate::theme::Role::Accent).add_modifier(ratatui::style::Modifier::BOLD)
}

fn dim() -> ratatui::style::Style {
    ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)
}

fn bold() -> ratatui::style::Style {
    ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD)
}

fn accent() -> ratatui::style::Style {
    crate::theme::style(crate::theme::Role::Accent)
}

fn rule() -> ratatui::style::Style {
    crate::theme::style(crate::theme::Role::Rule)
}

/// An answered form's rule at 14:16 UTC.
fn answered_at(ts: u64) -> super::Answered {
    super::Answered::new(
        &["Base".to_owned(), "Name".to_owned()],
        &[
            contract::events::FormAnswer::Answered {
                labels: vec!["dev".to_owned()],
                text: Some("and tags".to_owned()),
            },
            contract::events::FormAnswer::Skipped {
                skipped: contract::shapes::True,
            },
        ],
        Some("by friday"),
        ts,
    )
}

#[test]
fn the_rule_names_the_resolution_time_and_fills_the_width() {
    // 14:16 UTC: the rule reads "you answered · 14:16" and dashes fill
    // the 40 columns past it.
    let rows = drawn(&answered_at(51_360_000), 40, &jiff::tz::TimeZone::UTC);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        spans(&rows[0]),
        [
            ("you answered".to_owned(), accent_bold()),
            (" · 14:16 ".to_owned(), dim()),
            ("─".repeat(40 - 12 - 9), rule()),
        ]
    );
    assert_eq!(
        rows[0].to_string(),
        format!("you answered · 14:16 {}", "─".repeat(19))
    );
}

#[test]
fn a_narrow_rule_draws_no_dashes() {
    // "you answered" is 12 cells and " · 14:16 " 9: one below, exact
    // and one above the fit.
    let rows = drawn(&answered_at(51_360_000), 20, &jiff::tz::TimeZone::UTC);
    assert_eq!(spans(&rows[0]).len(), 2);
    let rows = drawn(&answered_at(51_360_000), 21, &jiff::tz::TimeZone::UTC);
    assert_eq!(spans(&rows[0]).len(), 2);
    let rows = drawn(&answered_at(51_360_000), 22, &jiff::tz::TimeZone::UTC);
    assert_eq!(
        spans(&rows[0]),
        [
            ("you answered".to_owned(), accent_bold()),
            (" · 14:16 ".to_owned(), dim()),
            ("─".to_owned(), rule()),
        ]
    );
}

#[test]
fn answers_draw_bold_and_skipped_and_the_note_draws_bold() {
    let rows = drawn(&answered_at(0), 80, &jiff::tz::TimeZone::UTC);
    assert_eq!(
        spans(&rows[1]),
        [
            ("Base         ".to_owned(), accent()),
            ("dev, \"and tags\"".to_owned(), bold()),
        ]
    );
    assert_eq!(
        spans(&rows[2]),
        [
            ("Name         ".to_owned(), accent()),
            ("skipped".to_owned(), dim())
        ]
    );
    assert_eq!(
        spans(&rows[3]),
        [
            ("note".to_owned(), accent()),
            (" ".to_owned(), ratatui::style::Style::default()),
            ("\"by friday\"".to_owned(), bold()),
        ]
    );
}

#[test]
fn a_declined_form_draws_one_dim_row_and_no_rule() {
    let rows = drawn(&super::Answered::declined(), 80, &jiff::tz::TimeZone::UTC);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].style, dim());
    assert_eq!(
        spans(&rows[0]),
        [(
            "declined: the turn ended so you could answer in your own words".to_owned(),
            ratatui::style::Style::default()
        )]
    );
}

/// `interaction_resolved` for `request` by `by` at `ts`, with `answer`'s keys.
fn resolved_at(request: &str, by: &str, answer: Value, ts: u64) -> contract::Envelope {
    let mut line = resolved(request, by, answer);
    line.ts = ts;
    line
}

#[test]
fn the_rule_keeps_the_resolution_time() {
    let mut lines = asking();
    lines.push(resolved_at("r_4f", "person", answers(), 51_360_000));
    let (turns, _) = folded(&lines);
    let ts = turns
        .iter()
        .flat_map(|turn| turn.entries.iter())
        .find_map(|entry| match entry {
            crate::turn::Entry::Answers(answered) => Some(answered),
            crate::turn::Entry::Reply { .. }
            | crate::turn::Entry::Steer(_)
            | crate::turn::Entry::Group(_)
            | crate::turn::Entry::Aside(_)
            | crate::turn::Entry::Band(_) => None,
        });
    let Some(super::Answered::Given { ts, .. }) = ts else {
        panic!("a rule with the resolution time");
    };
    assert_eq!(*ts, 51_360_000);
}

#[test]
fn a_person_declined_form_draws_the_declined_row() {
    let mut app = app();
    feed(&mut app, asking());
    feed(
        &mut app,
        [
            resolved("r_4f", "person", declined()),
            completed("a_1", "completed", 0),
        ],
    );
    let texts = texts(&app);
    let at = texts
        .iter()
        .position(|text| text.contains("declined:"))
        .expect("the declined row");
    // The row wraps at the card's width: it reads whole across rows.
    let row: String = texts
        .iter()
        .skip(at)
        .take(2)
        .map(|text| text.trim().to_owned())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        row,
        "declined: the turn ended so you could answer in your own words"
    );
    assert!(rules(&app).is_empty());
    assert!(ledger(&app).ends_with("declined"), "{}", ledger(&app));
}

#[test]
fn a_duplicate_decline_draws_one_row() {
    let mut lines = asking();
    lines.push(resolved("r_4f", "person", declined()));
    lines.push(resolved("r_4f", "person", declined()));
    let (_, changed) = folded(&lines);
    assert_eq!(changed, vec![true, true, true, true, false]);
    let mut app = app();
    feed(&mut app, lines);
    assert_eq!(
        texts(&app)
            .iter()
            .filter(|text| text.contains("declined:"))
            .count(),
        1
    );
}

#[test]
fn a_declined_page_loaded_from_history_draws_the_same_row() {
    let mut lines = asking();
    lines.push(resolved("r_4f", "person", declined()));
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
        live.iter().any(|(line, _)| line
            .to_string()
            .contains("declined: the turn ended so you could answer")),
        "the declined row is drawn live"
    );
    assert_eq!(pages.page_count(), 2);
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
    pages.load(&contract::SessionId(SESSION.to_owned()), &history);
    assert!(pages.part(0).is_some(), "the first page loaded again");
    assert_eq!(pages.rows(), live);
}

#[test]
fn the_answers_entry_carries_given_or_declined() {
    use crate::ext::TurnAnswers;
    let given = super::Answered::Given {
        parts: vec![crate::format::AnswerParts {
            header: "Base".to_owned(),
            text: "main".to_owned(),
            skipped: false,
        }],
        note: Some("go".to_owned()),
        ts: 7,
    };
    let TurnAnswers::Given { parts, note, ts } = given.turn_entry() else {
        panic!("a given entry");
    };
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].header, "Base");
    assert_eq!(parts[0].text, "main");
    assert!(!parts[0].skipped);
    assert_eq!(note, Some("go"));
    assert_eq!(ts, 7);
    let declined = super::Answered::Declined;
    assert!(matches!(declined.turn_entry(), TurnAnswers::Declined));
}
