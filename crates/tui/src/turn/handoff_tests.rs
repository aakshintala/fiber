//! Tests for the handoff band and the nudge, driven through the app.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::app::{App, Target};
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

fn feed(app: &mut App, kind: &str, action: Option<&str>, payload: Value) {
    app.on_line(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

fn start(app: &mut App) {
    feed(
        app,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
}

fn preamble(app: &mut App, trigger_at: Option<u64>) {
    let mut payload = json!({"reason": "start", "model": "fake/m", "context_window": 1_000_000,
        "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": []});
    if let (Some(at), Some(object)) = (trigger_at, payload.as_object_mut()) {
        object.insert("trigger_at".to_owned(), json!(at));
    }
    feed(app, "preamble_built", None, payload);
}

fn started(app: &mut App, trigger: &str) {
    feed(app, "handoff_started", None, json!({ "trigger": trigger }));
}

fn completed(app: &mut App, payload: Value) {
    feed(app, "handoff_completed", None, payload);
}

fn usage(app: &mut App, id: &str, input: u64, extra: Value) {
    let mut payload = json!({"generation_id": id, "model": "fake/m",
        "tokens": {"input": input, "cache_read": 2_000, "cache_write": {"5m": 1_000}, "output": 50},
        "input_bytes": 0, "cost": null});
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    feed(app, "usage_recorded", Some("a_x"), payload);
}

/// The band's line.
fn band(app: &App) -> String {
    texts(app)
        .into_iter()
        .find(|line| line.starts_with('⇄'))
        .unwrap_or_default()
}

#[test]
fn a_call_that_reported_no_input_tokens_leaves_the_size_waiting() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started(&mut app, "auto");
    completed(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
    );
    // A call that failed before its provider named a generation: no input
    // tokens, so no size, whatever its output.
    usage(
        &mut app,
        "fiber-0123456789abcdef",
        0,
        json!({"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 5}}),
    );
    assert_eq!(band(&app), "⇄ Handoff · automatic at 400.0k · 402.0k → …");
    usage(&mut app, "g_next", 29_000, json!({}));
    assert_eq!(
        band(&app),
        "⇄ Handoff · automatic at 400.0k · 402.0k → 32.0k"
    );
}

#[test]
fn each_trigger_reads_as_the_band_says_it() {
    for (trigger_at, trigger, says) in [
        (Some(400_000), "auto", "automatic at 400.0k"),
        (None, "auto", "automatic"),
        (Some(400_000), "person", "you asked with /handoff"),
        (Some(400_000), "overflow", "the request did not fit"),
    ] {
        let mut app = app();
        preamble(&mut app, trigger_at);
        start(&mut app);
        started(&mut app, trigger);
        assert_eq!(
            band(&app),
            format!("⇄ Handoff · {says} · ● writing the note…"),
            "{trigger}"
        );
    }
    // The latest `preamble_built` wins, an absent `trigger_at` included.
    let mut app = app();
    preamble(&mut app, Some(400_000));
    preamble(&mut app, None);
    start(&mut app);
    started(&mut app, "auto");
    assert_eq!(band(&app), "⇄ Handoff · automatic · ● writing the note…");
}

#[test]
fn the_note_goes_into_the_band_and_the_size_waits_for_the_next_call() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    feed(
        &mut app,
        "text_completed",
        Some("a_1"),
        json!({"text": "Working."}),
    );
    started(&mut app, "auto");
    feed(
        &mut app,
        "assistant_message_delta",
        Some("a_n"),
        json!({"text": "## Note"}),
    );
    feed(
        &mut app,
        "text_completed",
        Some("a_n"),
        json!({"text": "## Note\nkeep going"}),
    );
    // The note's own call sizes nothing: the handoff has not completed.
    usage(&mut app, "g_note", 1_000, json!({}));
    assert_eq!(
        texts(&app),
        [
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "00:00",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            "Working.",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "⇄ Handoff · automatic at 400.0k · ● writing the note…",
        ]
    );
    completed(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
    );
    assert_eq!(band(&app), "⇄ Handoff · automatic at 400.0k · 402.0k → …");
    // A correction to the note's call, a copy and an extension's call are
    // not the first request after it.
    usage(&mut app, "g_note", 5_000, json!({}));
    usage(
        &mut app,
        "g_copy",
        5_000,
        json!({"origin_session_id": "s_bbbbbbbbbbbbbbbb"}),
    );
    usage(&mut app, "g_ext", 5_000, json!({"extension": "x"}));
    assert_eq!(band(&app), "⇄ Handoff · automatic at 400.0k · 402.0k → …");
    usage(&mut app, "g_next", 29_000, json!({}));
    assert_eq!(
        band(&app),
        "⇄ Handoff · automatic at 400.0k · 402.0k → 32.0k"
    );
    // Only the first call after it sizes it.
    usage(&mut app, "g_later", 90_000, json!({}));
    assert_eq!(
        band(&app),
        "⇄ Handoff · automatic at 400.0k · 402.0k → 32.0k"
    );
    // A reply after the handoff is a reply again, in the second card.
    feed(
        &mut app,
        "assistant_message_delta",
        Some("a_2"),
        json!({"text": "Continuing."}),
    );
    feed(
        &mut app,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    let lines = texts(&app);
    assert_eq!(lines[8], "  ▸ note");
    assert_eq!(lines[10], "Continuing.");
    assert!(lines[11].starts_with("▣ completed"), "{lines:?}");
    // "▸ note" opens the note inside the band.
    let note = app
        .targets()
        .into_iter()
        .find_map(|(at, target)| (at == 8).then_some(target));
    assert!(matches!(note, Some(Target::Note(_))), "{note:?}");
    if let Some(note) = note {
        app.open(note);
        assert_eq!(
            texts(&app)[8..11],
            ["  ▸ note", "    ## Note", "    keep going"]
        );
        app.open(note);
        assert_eq!(texts(&app)[10], "Continuing.");
    }
}

#[test]
fn a_failed_or_cancelled_handoff_leaves_the_context_unchanged() {
    let mut app = app();
    start(&mut app);
    started(&mut app, "person");
    completed(
        &mut app,
        json!({"outcome": "failed", "tokens_before": 10_000,
            "error": {"code": "io_failed", "message": "The note request failed."}}),
    );
    assert_eq!(
        texts(&app)[4..],
        ["⇄ Handoff · you asked with /handoff · context unchanged · The note request failed."]
    );
    usage(&mut app, "g_next", 29_000, json!({}));
    started(&mut app, "overflow");
    completed(
        &mut app,
        json!({"outcome": "cancelled", "tokens_before": 10_000}),
    );
    assert_eq!(
        texts(&app)[5..],
        ["⇄ Handoff · the request did not fit · context unchanged"]
    );
}

#[test]
fn a_hooks_note_and_a_tool_started_handoff() {
    let mut app = app();
    start(&mut app);
    // The model's own tool writes no `handoff_started`.
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 300_000,
            "note_text": "From the hook.", "extension": "notes"}),
    );
    assert_eq!(
        texts(&app)[4..],
        ["⇄ Handoff · the model handed off · 300.0k → …", "  ▸ note"]
    );
    let note = app.targets().into_iter().map(|(_, t)| t).next();
    if let Some(note) = note {
        app.open(note);
    }
    assert_eq!(texts(&app)[6], "    From the hook.");
    // With no turn open there is no card to break.
    let mut app = self::app();
    started(&mut app, "person");
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1}),
    );
    assert!(texts(&app).is_empty());
}

#[test]
fn the_band_ends_the_open_group() {
    let mut app = app();
    start(&mut app);
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_1"),
        json!({"name": "read", "arguments": {"path": "a.rs"}}),
    );
    started(&mut app, "auto");
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_2"),
        json!({"name": "read", "arguments": {"path": "b.rs"}}),
    );
    let lines = texts(&app);
    assert_eq!(lines.len(), 11, "{lines:?}");
    assert!(lines[5].starts_with('•') && lines[9].starts_with('•'));
    assert!(lines[7].starts_with('⇄'));
}

#[test]
fn the_nudge_is_one_dim_line_where_it_happened() {
    let nudge = "◔ Context at 268.0k, two thirds of the way to the 400.0k handoff. The \
                 model was told a handoff keeps the work going.";
    let mut app = app();
    feed(
        &mut app,
        "context_nudged",
        None,
        json!({"tokens": 268_000, "trigger_at": 400_000}),
    );
    start(&mut app);
    feed(
        &mut app,
        "context_nudged",
        None,
        json!({"tokens": 268_000, "trigger_at": 400_000}),
    );
    assert_eq!(
        texts(&app),
        [
            "◔ Context at 268.0k, two thirds of the way to the 400.0k handoff. The model was told a handoff keeps the work going.",
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "00:00",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            "◔ Context at 268.0k, two thirds of the way to the 400.0k handoff. The model was told a handoff keeps the work going.",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
        ]
    );
    assert!(
        app.lines()
            .iter()
            .filter(|line| line.to_string() == nudge)
            .all(|line| line
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM))
    );
}

/// A band whose note streamed as `deltas` (action and text) and then
/// `done` (action and full text), completed; its note opened.
fn note_of(deltas: &[(&str, &str)], done: &[(&str, &str)]) -> Vec<String> {
    let mut app = app();
    start(&mut app);
    started(&mut app, "person");
    for (action, text) in deltas {
        feed(
            &mut app,
            "assistant_message_delta",
            Some(action),
            json!({ "text": text }),
        );
    }
    for (action, text) in done {
        feed(
            &mut app,
            "text_completed",
            Some(action),
            json!({ "text": text }),
        );
    }
    completed(
        &mut app,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
    let note = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Note(_)).then_some(target));
    assert!(note.is_some(), "{:?}", texts(&app));
    if let Some(note) = note {
        app.open(note);
    }
    texts(&app)[6..].to_vec()
}

#[test]
fn a_streamed_note_joins_each_parts_deltas() {
    // A part's deltas join; the note keeps them with no completion.
    assert_eq!(
        note_of(&[("a_1", "## No"), ("a_1", "te")], &[]),
        ["    ## Note"]
    );
    // Each part streams on its own.
    assert_eq!(
        note_of(&[("a_1", "one"), ("a_2", "two"), ("a_1", " more")], &[]),
        ["    one more", "    two"]
    );
    // A completion replaces its own part only.
    assert_eq!(
        note_of(&[("a_1", "one")], &[("a_2", "two")]),
        ["    one", "    two"]
    );
}

#[test]
fn set_open_reports_whether_the_note_matched() {
    let mut band = super::Band::new(3, "the model handed off".to_owned());
    // The matching target sets the flag and answers true, both ways.
    assert!(band.set_open(&Target::Note(3), true));
    assert!(band.open);
    assert!(band.set_open(&Target::Note(3), false));
    assert!(!band.open);
    // Another band's note and another kind answer false and change nothing.
    assert!(!band.set_open(&Target::Note(4), true));
    assert!(!band.open);
    assert!(!band.set_open(&Target::Orphans(3), true));
    assert!(!band.open);
}

#[test]
fn a_writing_band_is_marked_at_its_dot() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started(&mut app, "auto");
    // The dot after "⇄ Handoff · automatic at 400.0k · " spins.
    let shown = app.shown(0, usize::MAX);
    let at = shown
        .lines
        .iter()
        .position(|(line, _, _)| line.to_string().starts_with('⇄'))
        .expect("a band line");
    assert_eq!(shown.spins, vec![(at, 34)]);
}

#[test]
fn a_finished_band_is_not_marked() {
    let mut app = app();
    preamble(&mut app, Some(400_000));
    start(&mut app);
    started(&mut app, "auto");
    completed(
        &mut app,
        json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
    );
    assert!(band(&app).starts_with("⇄ Handoff"));
    assert_eq!(app.shown(0, usize::MAX).spins, Vec::new());
}
