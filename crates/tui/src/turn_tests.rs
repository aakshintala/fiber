//! Tests for the turn fold: cards, groups, the ledger, thinking and the ▣
//! line, driven through the app as the hub's lines arrive.

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::style::Modifier;
use serde_json::{Value, json};

use crate::app::{App, Target};
use crate::keys::Key;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// An app attached to [`SESSION`], 60 columns wide.
fn app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 24);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Folds one line of `kind` at `ts` milliseconds.
fn feed(app: &mut App, kind: &str, action: Option<&str>, ts: u64, payload: Value) {
    app.on_line(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

fn start(app: &mut App, text: &str, ts: u64) {
    feed(
        app,
        "turn_started",
        None,
        ts,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    );
}

fn end(app: &mut App, outcome: &str, ts: u64) {
    let payload = if outcome == "failed" {
        json!({"outcome": outcome, "error": {"code": "io_failed", "message": "boom"}})
    } else {
        json!({"outcome": outcome})
    };
    feed(app, "turn_completed", None, ts, payload);
}

fn step(app: &mut App, ts: u64) {
    feed(app, "step_started", None, ts, json!({}));
}

fn text(app: &mut App, message: &str, text: &str, ts: u64) {
    feed(
        app,
        "text_completed",
        Some(message),
        ts,
        json!({ "text": text }),
    );
}

fn delta(app: &mut App, message: &str, text: &str, ts: u64) {
    feed(
        app,
        "assistant_message_delta",
        Some(message),
        ts,
        json!({ "text": text }),
    );
}

fn think(app: &mut App, action: &str, text: &str, from: u64, to: u64) {
    feed(app, "reasoning_started", Some(action), from, json!({}));
    feed(
        app,
        "reasoning_completed",
        Some(action),
        to,
        json!({ "text": text }),
    );
}

fn request(app: &mut App, action: &str, name: &str, arguments: Value, ts: u64) {
    feed(
        app,
        "tool_call_requested",
        Some(action),
        ts,
        json!({"name": name, "arguments": arguments}),
    );
}

fn complete(app: &mut App, action: &str, ts: u64, extra: Value) {
    let mut payload = json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]});
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    feed(app, "tool_call_completed", Some(action), ts, payload);
}

/// A call that ran to completion.
fn call(app: &mut App, action: &str, name: &str, arguments: Value, ts: u64) {
    request(app, action, name, arguments, ts);
    complete(app, action, ts, json!({}));
}

fn usage(app: &mut App, id: &str, tokens: u64, cost: Value, extra: Value) {
    let mut payload = json!({"generation_id": id, "model": "fake/m",
        "tokens": {"input": tokens, "cache_read": 0, "cache_write": {}, "output": 0},
        "input_bytes": 0, "cost": cost});
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    feed(app, "usage_recorded", Some("a_m"), 0, payload);
}

fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

fn last(app: &App) -> String {
    texts(app).pop().unwrap_or_default()
}

/// The line that reads `text`.
fn styled(app: &App, text: &str) -> ratatui::text::Line<'static> {
    app.lines()
        .into_iter()
        .find(|line| line.to_string() == text)
        .unwrap_or_default()
}

fn dim(line: &ratatui::text::Line<'_>) -> bool {
    line.style.add_modifier.contains(Modifier::DIM)
}

fn bold(line: &ratatui::text::Line<'_>) -> bool {
    line.style.add_modifier.contains(Modifier::BOLD)
}

/// An action no line names.
fn none() -> usize {
    usize::MAX
}

/// The first group target.
fn group(app: &App) -> Target {
    app.targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Group(_)).then_some(target))
        .unwrap_or(Target::Group(none()))
}

/// The target of the line that reads `text`.
fn target(app: &App, text: &str) -> Target {
    let lines = texts(app);
    app.targets()
        .into_iter()
        .find_map(|(at, target)| {
            (lines.get(at).map(String::as_str) == Some(text)).then_some(target)
        })
        .unwrap_or(Target::Group(none()))
}

fn ctrl_o(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlO, now);
}

#[test]
fn a_group_counts_its_calls_by_kind_and_its_span() {
    let mut app = app();
    start(&mut app, "fix it", 0);
    step(&mut app, 0);
    think(&mut app, "a_t", "## Plan", 1_000, 2_000);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 3_000);
    call(&mut app, "a_2", "read", json!({"path": "b.rs"}), 4_000);
    request(&mut app, "a_3", "edit", json!({"path": "a.rs"}), 5_000);
    complete(
        &mut app,
        "a_3",
        6_000,
        json!({"changes": [{"path": "a.rs", "added": 3, "removed": 1}]}),
    );
    call(
        &mut app,
        "a_4",
        "shell",
        json!({"command": "cargo test"}),
        13_000,
    );
    text(&mut app, "a_m", "Done.", 14_000);
    assert_eq!(
        texts(&app),
        vec![
            " fix it ",
            "00:00",
            "• Read 2 files, edited 1 file +3 −1, ran 1 command, thought once · 12s",
            "Done.",
        ]
    );
    let summary = styled(&app, &texts(&app)[2]);
    assert!(dim(&summary));
    assert!(!dim(&styled(&app, "Done.")));
}

#[test]
fn shell_searches_and_other_tools_have_their_own_kinds() {
    let mut app = app();
    start(&mut app, "look", 0);
    call(
        &mut app,
        "a_1",
        "shell",
        json!({"command": "rg foo src"}),
        0,
    );
    call(&mut app, "a_2", "shell", json!({"command": "grep -r x"}), 0);
    call(
        &mut app,
        "a_3",
        "shell",
        json!({"command": "find . -name y"}),
        0,
    );
    call(&mut app, "a_4", "shell", json!({"command": "ls"}), 0);
    call(&mut app, "a_5", "shell", json!({}), 0);
    call(&mut app, "a_6", "web_fetch", json!({"url": "u"}), 0);
    call(&mut app, "a_7", "write", json!({"path": "n.rs"}), 0);
    call(&mut app, "a_8", "edit", json!({"path": "n.rs"}), 0);
    text(&mut app, "a_m", "ok", 0);
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Searched 3 patterns, edited 2 files, ran 2 commands, 1 other call")
    );
}

#[test]
fn edited_files_count_distinct_paths_and_sum_their_lines() {
    let mut app = app();
    start(&mut app, "edit", 0);
    for (action, path, added) in [("a_1", "a.rs", 1), ("a_2", "a.rs", 2), ("a_3", "b.rs", 4)] {
        request(&mut app, action, "edit", json!({ "path": path }), 0);
        complete(
            &mut app,
            action,
            0,
            json!({"changes": [{"path": path, "added": added, "removed": 1}]}),
        );
    }
    // A failed edit with no changes adds no file once others changed some.
    request(&mut app, "a_4", "edit", json!({"path": "c.rs"}), 0);
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_4"),
        0,
        json!({"status": "failed", "content": [], "error": {"code": "io_failed", "message": "no"}}),
    );
    text(&mut app, "a_m", "ok", 0);
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Edited 2 files +7 −3")
    );
}

#[test]
fn text_splits_groups_and_steering_does_not() {
    let mut app = app();
    start(&mut app, "go", 0);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    feed(
        &mut app,
        "steering_applied",
        None,
        0,
        json!({"content": [{"type": "text", "text": "also b"}], "source": "driver"}),
    );
    call(&mut app, "a_2", "read", json!({"path": "b.rs"}), 0);
    text(&mut app, "a_m", "Read them.", 0);
    // An empty text part shows nothing and ends nothing.
    call(&mut app, "a_3", "read", json!({"path": "c.rs"}), 0);
    text(&mut app, "a_n", "", 0);
    call(&mut app, "a_4", "read", json!({"path": "d.rs"}), 0);
    text(&mut app, "a_n", "More.", 0);
    end(&mut app, "completed", 0);
    assert_eq!(
        texts(&app),
        vec![
            " go ",
            "00:00",
            "• Read 2 files",
            "steer · also b",
            "Read them.",
            "• Read 2 files",
            "More.",
            "▣ completed · 4 calls",
        ]
    );
}

#[test]
fn a_thinking_only_group_is_one_line_per_block() {
    let mut app = app();
    start(&mut app, "why", 0);
    think(&mut app, "a_t", "**Plan the fix**\nfirst a", 0, 22_000);
    think(&mut app, "a_u", "short", 22_000, 22_999);
    text(&mut app, "a_m", "Because.", 23_000);
    assert_eq!(
        texts(&app),
        vec![
            " why ",
            "00:00",
            "+ Thought: Plan the fix · 22s",
            "+ Thought: short",
            "Because."
        ]
    );
    assert!(dim(&styled(&app, "+ Thought: short")));
    // Opening a thought shows its text under it, dim.
    let thought = target(&app, "+ Thought: Plan the fix · 22s");
    app.open(thought);
    assert_eq!(
        texts(&app).get(3..5).map(<[String]>::to_vec),
        Some(vec!["**Plan the fix**".to_owned(), "first a".to_owned()])
    );
    assert!(dim(&styled(&app, "first a")));
    app.open(thought);
    assert_eq!(texts(&app).len(), 5);
}

#[test]
fn a_thought_with_no_end_or_text_has_no_figures() {
    let mut app = app();
    start(&mut app, "q", 0);
    feed(&mut app, "reasoning_started", Some("a_t"), 0, json!({}));
    text(&mut app, "a_m", "A.", 5_000);
    assert_eq!(texts(&app).get(2).map(String::as_str), Some("+ Thought"));
}

#[test]
fn a_running_group_shows_calls_in_flight_and_thinking() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    request(&mut app, "a_2", "shell", json!({"command": "cargo t"}), 0);
    feed(&mut app, "tool_call_started", Some("a_2"), 0, json!({}));
    feed(&mut app, "reasoning_started", Some("a_t"), 0, json!({}));
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 1 file, ran 1 command, thought once · shell cargo t · Thinking")
    );
    feed(
        &mut app,
        "reasoning_delta",
        Some("a_t"),
        0,
        json!({"text": "# One\nx\n**Two**"}),
    );
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 1 file, ran 1 command, thought once · shell cargo t · Thinking: Two")
    );
    // A block that has finished no longer shows; completed calls leave.
    feed(
        &mut app,
        "reasoning_completed",
        Some("a_t"),
        0,
        json!({"text": "# One"}),
    );
    complete(&mut app, "a_2", 0, json!({}));
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 1 file, ran 1 command, thought once")
    );
}

#[test]
fn raw_arguments_stream_until_their_call_is_requested() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "assistant_message_started",
        Some("a_m"),
        0,
        json!({}),
    );
    let args = |index: u32, name: Option<&str>, text: &str| {
        let mut payload = json!({"index": index, "text": text});
        if let (Some(object), Some(name)) = (payload.as_object_mut(), name) {
            object.insert("name".to_owned(), json!(name));
        }
        payload
    };
    for (index, name, text) in [
        (1, None, "{\"co"),
        (0, Some("read"), "{\"pa"),
        (0, None, "th\""),
        (1, Some("shell"), "m"),
    ] {
        feed(
            &mut app,
            "tool_call_arguments_delta",
            Some("a_m"),
            0,
            args(index, name, text),
        );
    }
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• shell {\"com, read {\"path\"")
    );
    // The requested call takes the place of the lowest index.
    request(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 1 file · read a.rs, shell {\"com")
    );
    request(&mut app, "a_2", "shell", json!({"command": "ls"}), 0);
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 1 file, ran 1 command · read a.rs, shell ls")
    );
}

#[test]
fn a_streaming_call_with_no_name_yet_shows_an_ellipsis() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "tool_call_arguments_delta",
        Some("a_m"),
        0,
        json!({"index": 0, "text": "{"}),
    );
    assert_eq!(texts(&app).get(2).map(String::as_str), Some("• … {"));
    // A failed message stops streaming: nothing is left in flight.
    feed(
        &mut app,
        "assistant_message_completed",
        Some("a_m"),
        0,
        json!({"outcome": "failed"}),
    );
    assert_eq!(texts(&app), vec![" go ", "00:00"]);
}

/// A `turn_started` with `texts` messages at `ts` milliseconds.
fn start_all(app: &mut App, texts: &[&str], ts: u64) {
    feed(
        app,
        "turn_started",
        None,
        ts,
        json!({"input": texts.iter().map(|text| {
            json!({"type": "message", "source": "driver",
                "content": [{"type": "text", "text": text}]})
        }).collect::<Vec<_>>()}),
    );
}

#[test]
fn each_prompt_bubble_carries_the_turn_start_time() {
    let mut app = app();
    start_all(&mut app, &["one", "two"], 1791468900000);
    // The default zone is UTC: 2026-10-08T14:15Z.
    assert_eq!(texts(&app), vec![" one ", "14:15", " two ", "14:15"]);
    let times = texts(&app)
        .iter()
        .filter(|line| line.as_str() == "14:15")
        .count();
    assert_eq!(times, 2);
}

#[test]
fn a_prompt_with_no_bubble_has_no_time_row() {
    let mut app = app();
    start_all(&mut app, &["  ", "go"], 0);
    assert_eq!(texts(&app), vec![" go ", "00:00"]);
}

#[test]
fn a_call_requested_with_no_deltas_joins_the_open_group() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "assistant_message_started",
        Some("a_m"),
        0,
        json!({}),
    );
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    call(&mut app, "a_2", "read", json!({"path": "b.rs"}), 0);
    assert_eq!(
        texts(&app).get(2).map(String::as_str),
        Some("• Read 2 files")
    );
}

#[test]
fn live_text_streams_ahead_of_the_calls_it_precedes() {
    // Live, a message's deltas all arrive before its durable lines: text,
    // then a call's arguments, then more text.
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "assistant_message_started",
        Some("a_m"),
        0,
        json!({}),
    );
    delta(&mut app, "a_m", "First.", 0);
    feed(
        &mut app,
        "tool_call_arguments_delta",
        Some("a_m"),
        0,
        json!({"index": 0, "name": "read", "text": "{}"}),
    );
    delta(&mut app, "a_m", "Second.", 0);
    text(&mut app, "a_m", "First.", 0);
    request(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    text(&mut app, "a_m", "Second.", 0);
    assert_eq!(
        texts(&app),
        vec![" go ", "00:00", "First.", "• Read 1 file", "Second."]
    );
}

#[test]
fn the_closing_line_carries_every_figure_but_zero_ones() {
    let mut app = app();
    start(&mut app, "go", 1_000);
    for n in 0..12 {
        call(
            &mut app,
            &format!("a_{n}"),
            "read",
            json!({"path": "a.rs"}),
            2_000,
        );
    }
    usage(&mut app, "g1", 18_200, json!(0.41), json!({}));
    usage(
        &mut app,
        "g2",
        0,
        json!(1.10),
        json!({"subscription": true}),
    );
    end(&mut app, "completed", 39_000);
    assert_eq!(
        last(&app),
        "▣ completed · 38s · 12 calls · 18.2k tokens · $0.41 · $1.10 on subscription"
    );
    assert!(dim(&styled(&app, &last(&app))));
}

#[test]
fn a_model_with_no_price_shows_tokens_only() {
    let mut app = app();
    start(&mut app, "go", 0);
    usage(&mut app, "g1", 340, Value::Null, json!({}));
    end(&mut app, "completed", 2_000);
    assert_eq!(last(&app), "▣ completed · 2s · 340 tokens");
}

#[test]
fn every_kind_of_token_counts_and_one_call_is_singular() {
    let mut app = app();
    start(&mut app, "go", 0);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    feed(
        &mut app,
        "usage_recorded",
        Some("a_m"),
        0,
        json!({"generation_id": "g1", "model": "fake/m",
            "tokens": {"input": 1, "cache_read": 10, "cache_write": {"5m": 100, "1h": 1000},
                "output": 10000},
            "input_bytes": 0, "cost": 0.004}),
    );
    end(&mut app, "interrupted", 999);
    assert_eq!(last(&app), "▣ interrupted · 1 call · 11.1k tokens · <$0.01");
}

#[test]
fn a_subscription_only_turn_shows_no_billed_figure() {
    let mut app = app();
    start(&mut app, "go", 0);
    usage(&mut app, "g1", 5, json!(0.2), json!({"subscription": true}));
    end(&mut app, "failed", 0);
    assert_eq!(last(&app), "▣ failed · 5 tokens · $0.20 on subscription");
}

#[test]
fn billed_calls_with_no_known_cost_add_nothing_to_known_ones() {
    let mut app = app();
    start(&mut app, "go", 0);
    usage(&mut app, "g1", 5, Value::Null, json!({}));
    usage(&mut app, "g2", 5, json!(0.5), json!({}));
    end(&mut app, "completed", 0);
    assert_eq!(last(&app), "▣ completed · 10 tokens · $0.50");
}

#[test]
fn a_generation_counts_once_and_its_latest_line_wins() {
    let mut app = app();
    start(&mut app, "go", 0);
    usage(&mut app, "g1", 100, json!(0.1), json!({}));
    // A delegate's copy of its own call, and a copy of that copy.
    usage(
        &mut app,
        "g2",
        200,
        json!(0.2),
        json!({"origin_session_id": "s_b"}),
    );
    usage(
        &mut app,
        "g2",
        200,
        json!(0.2),
        json!({"origin_session_id": "s_b"}),
    );
    end(&mut app, "completed", 0);
    assert_eq!(last(&app), "▣ completed · 300 tokens · $0.30");
    // A late correction updates the closed card, and starts no other.
    start(&mut app, "next", 0);
    usage(&mut app, "g1", 900, json!(0.9), json!({}));
    end(&mut app, "completed", 0);
    let lines = texts(&app);
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("▣ completed · 1.1k tokens · $1.10")
    );
    assert_eq!(lines.last().map(String::as_str), Some("▣ completed"));
}

#[test]
fn usage_with_no_open_turn_and_an_unknown_id_is_dropped() {
    let mut app = app();
    start(&mut app, "go", 0);
    end(&mut app, "completed", 0);
    usage(&mut app, "g1", 100, json!(0.1), json!({}));
    start(&mut app, "next", 0);
    end(&mut app, "completed", 0);
    assert_eq!(
        texts(&app),
        vec![
            " go ",
            "00:00",
            "▣ completed",
            " next ",
            "00:00",
            "▣ completed"
        ]
    );
}

/// A turn with one group of two steps: a thought and a read, then an edit,
/// a failed, a denied, a cancelled and a running call.
fn ledger_app() -> App {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    think(&mut app, "a_t", "# Look first", 0, 4_000);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    step(&mut app, 0);
    request(&mut app, "a_2", "edit", json!({"path": "src/a.rs"}), 0);
    complete(
        &mut app,
        "a_2",
        0,
        json!({"changes": [{"path": "src/a.rs", "added": 3, "removed": 1}],
            "details": {"diff": "-old\n+new"}}),
    );
    request(&mut app, "a_3", "shell", json!({"command": "cargo t"}), 0);
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_3"),
        0,
        json!({"status": "failed", "content": [{"type": "text", "text": "out"}],
            "error": {"code": "nonzero_exit", "message": "exit 101"}}),
    );
    request(&mut app, "a_4", "web_fetch", json!({"url": "u"}), 0);
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_4"),
        0,
        json!({"status": "denied", "reason": "not allowed", "content": []}),
    );
    request(&mut app, "a_5", "custom", json!("raw text"), 0);
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_5"),
        0,
        json!({"status": "cancelled", "content": [{"type": "text", "text": "x"}]}),
    );
    request(&mut app, "a_6", "read", json!({"path": "z.rs"}), 0);
    text(&mut app, "a_m", "Done.", 0);
    app
}

#[test]
fn the_ledger_is_one_row_per_call_split_by_step() {
    let mut app = ledger_app();
    let summary = texts(&app).get(2).cloned().unwrap_or_default();
    app.open(group(&app));
    assert_eq!(
        texts(&app),
        vec![
            " go ".to_owned(),
            "00:00".to_owned(),
            summary,
            "  1 + Thought: Look first · 4s".to_owned(),
            "    read a.rs".to_owned(),
            "  2 edit src/a.rs +3 −1".to_owned(),
            "    shell cargo t · failed".to_owned(),
            "    web_fetch {\"url\":\"u\"} · denied".to_owned(),
            "    custom raw text · cancelled".to_owned(),
            "    read z.rs · running".to_owned(),
            "Done.".to_owned(),
        ]
    );
    // An edited row stands out; the rest sit back.
    let edited = styled(&app, "  2 edit src/a.rs +3 −1");
    assert!(bold(&edited) && !dim(&edited));
    for row in [
        "    read a.rs",
        "    shell cargo t · failed",
        "  1 + Thought: Look first · 4s",
    ] {
        let line = styled(&app, row);
        assert!(dim(&line) && !bold(&line), "{row}");
    }
    app.open(group(&app));
    assert_eq!(texts(&app).len(), 4);
}

#[test]
fn opening_a_call_shows_its_error_reason_diff_or_output() {
    let mut app = ledger_app();
    app.open(group(&app));
    for (row, shown) in [
        ("  2 edit src/a.rs +3 −1", vec!["    -old", "    +new"]),
        ("    shell cargo t · failed", vec!["    exit 101"]),
        (
            "    web_fetch {\"url\":\"u\"} · denied",
            vec!["    not allowed"],
        ),
        ("    custom raw text · cancelled", vec!["    x"]),
        ("    read a.rs", vec!["    ok"]),
    ] {
        let call = target(&app, row);
        app.open(call);
        let lines = texts(&app);
        let at = lines
            .iter()
            .position(|line| line == row)
            .unwrap_or_default();
        assert_eq!(
            lines
                .get(at + 1..at + 1 + shown.len())
                .map(<[String]>::to_vec),
            Some(shown.iter().map(|s| (*s).to_owned()).collect()),
            "{row}"
        );
        assert!(dim(&styled(
            &app,
            shown.first().copied().unwrap_or_default()
        )));
        app.open(call);
    }
    // A thought opened in the ledger shows its text under the gutter.
    app.open(target(&app, "  1 + Thought: Look first · 4s"));
    assert!(texts(&app).contains(&"    # Look first".to_owned()));
}

#[test]
fn targets_name_the_lines_they_open() {
    let mut app = ledger_app();
    assert_eq!(app.targets().len(), 1);
    app.open(group(&app));
    let targets = app.targets();
    // The group line, one thought and six calls; targets are unique.
    assert_eq!(targets.len(), 8);
    let mut identities: Vec<Target> = targets.iter().map(|(_, target)| *target).collect();
    identities.sort_unstable();
    identities.dedup();
    assert_eq!(identities.len(), 8);
    assert_eq!(targets.first().map(|(at, _)| *at), Some(2));
    // Opening something that is not there changes nothing.
    let before = texts(&app);
    app.open(Target::Call(none()));
    app.open(Target::Thought(none()));
    app.open(Target::Group(none()));
    assert_eq!(texts(&app), before);
}

#[test]
fn ctrl_o_opens_every_ledger_unless_all_are_open() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    text(&mut app, "a_m", "One.", 0);
    call(&mut app, "a_2", "read", json!({"path": "b.rs"}), 0);
    text(&mut app, "a_m", "Two.", 0);
    // A thinking-only group has no ledger and does not count.
    think(&mut app, "a_t", "hm", 0, 0);
    text(&mut app, "a_m", "Three.", 0);
    // One open, one closed: Ctrl+O opens both.
    app.open(group(&app));
    ctrl_o(&mut app);
    assert!(texts(&app).contains(&"  1 read a.rs".to_owned()));
    assert!(texts(&app).contains(&"  1 read b.rs".to_owned()));
    // All open: Ctrl+O closes them all.
    ctrl_o(&mut app);
    assert_eq!(texts(&app).len(), 8);
    // A group made later starts the way the last Ctrl+O left them.
    ctrl_o(&mut app);
    call(&mut app, "a_3", "read", json!({"path": "c.rs"}), 0);
    assert!(texts(&app).contains(&"  1 read c.rs".to_owned()));
}

#[test]
fn ctrl_o_with_no_ledger_yet_sets_how_groups_start() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    ctrl_o(&mut app);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    assert!(texts(&app).contains(&"  1 read a.rs".to_owned()));
    let mut app = self::app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    ctrl_o(&mut app);
    ctrl_o(&mut app);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    assert_eq!(texts(&app).len(), 3);
}

#[test]
fn an_open_approval_shows_its_group_ledger_until_resolved() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    request(&mut app, "a_1", "shell", json!({"command": "rm x"}), 0);
    feed(&mut app, "permission_requested", Some("a_1"), 0, json!({}));
    assert!(texts(&app).contains(&"  1 shell rm x · running".to_owned()));
    feed(&mut app, "permission_resolved", Some("a_1"), 0, json!({}));
    assert_eq!(texts(&app).len(), 3);
    // A group the person opened stays open after the answer.
    app.open(group(&app));
    feed(&mut app, "permission_requested", Some("a_1"), 0, json!({}));
    feed(&mut app, "permission_resolved", Some("a_1"), 0, json!({}));
    assert_eq!(texts(&app).len(), 4);
    // A request for a call the fold never saw changes nothing.
    app.open(group(&app));
    feed(&mut app, "permission_requested", Some("a_9"), 0, json!({}));
    assert_eq!(texts(&app).len(), 3);
}

#[test]
fn lines_the_fold_cannot_place_are_skipped() {
    let mut app = app();
    // No turn yet.
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    delta(&mut app, "a_m", "lost", 0);
    start(&mut app, "go", 0);
    // Unknown actions, unknown kinds and payloads that do not parse.
    feed(
        &mut app,
        "reasoning_delta",
        Some("a_x"),
        0,
        json!({"text": "t"}),
    );
    feed(
        &mut app,
        "reasoning_completed",
        Some("a_x"),
        0,
        json!({"text": "t"}),
    );
    feed(&mut app, "tool_call_started", Some("a_x"), 0, json!({}));
    complete(&mut app, "a_x", 0, json!({}));
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_2"),
        0,
        json!({"name": 3}),
    );
    feed(&mut app, "some_new_kind", Some("a_2"), 0, json!({}));
    feed(
        &mut app,
        "text_completed",
        None,
        0,
        json!({"text": "no action"}),
    );
    // A repeated start for one block keeps one block.
    feed(&mut app, "reasoning_started", Some("a_t"), 0, json!({}));
    feed(&mut app, "reasoning_started", Some("a_t"), 0, json!({}));
    end(&mut app, "completed", 0);
    assert_eq!(
        texts(&app),
        vec![" go ", "00:00", "+ Thought", "▣ completed"]
    );
}

#[test]
fn repaired_arguments_are_the_ones_shown() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    feed(
        &mut app,
        "tool_call_requested",
        Some("a_1"),
        0,
        json!({"name": "read", "arguments": {"path": null},
            "repaired": {"path": "fixed.rs"},
            "repairs": [{"path": "/path", "fix": "string_parsed"}]}),
    );
    ctrl_o(&mut app);
    assert!(texts(&app).contains(&"  1 read fixed.rs · running".to_owned()));
}

#[test]
fn the_prompt_is_a_tinted_bubble_on_the_right_at_most_seventy_percent_wide() {
    let mut app = app();
    let long = "word ".repeat(20);
    start(&mut app, long.trim(), 0);
    let lines = app.lines();
    // 60 columns: at most 42 columns, its text wrapped at 40; eight
    // words fill 39.
    assert_eq!(lines.len(), 4);
    for line in &lines[..3] {
        assert_eq!(
            line.alignment,
            Some(ratatui::layout::HorizontalAlignment::Right)
        );
        assert_eq!(line.width(), 41);
        assert!(line.spans.iter().all(|span| span.style.bg.is_some()));
    }
    let time = lines.last().cloned().unwrap_or_default();
    assert_eq!(time.to_string(), "00:00");
    assert_eq!(
        time.alignment,
        Some(ratatui::layout::HorizontalAlignment::Right)
    );
    assert!(time.style.add_modifier.contains(Modifier::DIM));
    assert_eq!(
        lines.first().map(ToString::to_string).as_deref(),
        Some(" word word word word word word word word ")
    );
    assert_eq!(
        lines.get(2).map(ToString::to_string).as_deref(),
        Some(format!(" word word word word{} ", " ".repeat(20)).as_str())
    );
}

#[test]
fn a_narrow_screen_still_draws_a_bubble() {
    let mut app = app();
    app.set_size(4, 10);
    start(&mut app, "hello", 0);
    // Three columns at least: one of text between the padding.
    assert_eq!(
        texts(&app),
        vec![" h ", " e ", " l ", " l ", " o ", "00:00"]
    );
}

#[test]
fn an_empty_prompt_draws_no_bubble() {
    let mut app = app();
    start(&mut app, "  ", 0);
    end(&mut app, "completed", 0);
    assert_eq!(texts(&app), vec!["▣ completed"]);
}

#[test]
fn a_streaming_call_with_no_text_yet_shows_its_name_alone() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "tool_call_arguments_delta",
        Some("a_m"),
        0,
        json!({"index": 0, "name": "read", "text": ""}),
    );
    assert_eq!(texts(&app).get(2).map(String::as_str), Some("• read"));
}

#[test]
fn ctrl_o_closes_when_every_ledger_is_open_whatever_thinking_groups_say() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    think(&mut app, "a_t", "hm", 0, 0);
    text(&mut app, "a_m", "Zero.", 0);
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    text(&mut app, "a_m", "One.", 0);
    call(&mut app, "a_2", "read", json!({"path": "b.rs"}), 0);
    text(&mut app, "a_m", "Two.", 0);
    let groups: Vec<Target> = app
        .targets()
        .into_iter()
        .filter_map(|(_, target)| matches!(target, Target::Group(_)).then_some(target))
        .collect();
    for group in groups {
        app.open(group);
    }
    assert!(texts(&app).contains(&"  1 read b.rs".to_owned()));
    ctrl_o(&mut app);
    assert!(!texts(&app).contains(&"  1 read a.rs".to_owned()));
    assert!(!texts(&app).contains(&"  1 read b.rs".to_owned()));
}

/// Folds one line while scrolled up; whether it raised the overlay.
fn flags(app: &mut App, kind: &str, action: Option<&str>, payload: Value) -> bool {
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::PageUp, now);
    feed(app, kind, action, 0, payload);
    let new = app.has_new();
    app.on_key(Key::End, now);
    new
}

#[test]
fn only_a_line_that_changes_a_card_raises_the_overlay() {
    let mut app = app();
    let usage = json!({"generation_id": "g_1", "model": "fake/m",
        "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 0},
        "input_bytes": 0, "cost": null});
    let steer = json!({"content": [{"type": "text", "text": "x"}], "source": "driver"});
    // With no turn open, nothing has a card to change.
    assert!(!flags(&mut app, "notice", None, json!({"message": "hi"})));
    assert!(!flags(
        &mut app,
        "usage_recorded",
        Some("a_m"),
        usage.clone()
    ));
    assert!(!flags(&mut app, "steering_applied", None, steer.clone()));
    assert!(!flags(
        &mut app,
        "turn_completed",
        None,
        json!({"outcome": "completed"})
    ));
    let prompt = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    assert!(flags(&mut app, "turn_started", None, prompt));
    assert!(!flags(&mut app, "step_started", None, json!({})));
    assert!(flags(&mut app, "steering_applied", None, steer));
    assert!(flags(&mut app, "usage_recorded", Some("a_m"), usage));
    // Lines about an action the fold never saw, or that add nothing.
    assert!(!flags(
        &mut app,
        "tool_call_started",
        Some("a_9"),
        json!({})
    ));
    let done = json!({"status": "completed", "content": []});
    assert!(!flags(
        &mut app,
        "tool_call_completed",
        Some("a_9"),
        done.clone()
    ));
    assert!(!flags(
        &mut app,
        "permission_requested",
        Some("a_9"),
        json!({})
    ));
    let thought = json!({"text": "t"});
    assert!(!flags(
        &mut app,
        "reasoning_completed",
        Some("r_9"),
        thought.clone()
    ));
    assert!(!flags(
        &mut app,
        "reasoning_delta",
        Some("r_9"),
        thought.clone()
    ));
    assert!(!flags(
        &mut app,
        "assistant_message_started",
        Some("a_m"),
        json!({})
    ));
    assert!(!flags(
        &mut app,
        "assistant_message_delta",
        Some("a_m"),
        json!({"text": ""})
    ));
    assert!(!flags(
        &mut app,
        "text_completed",
        Some("a_m"),
        json!({"text": ""})
    ));
    assert!(!flags(
        &mut app,
        "assistant_message_completed",
        Some("a_m"),
        json!({})
    ));
    assert!(!flags(
        &mut app,
        "tool_call_requested",
        Some("a_1"),
        json!({})
    ));
    assert!(!flags(
        &mut app,
        "model_call_failed",
        Some("a_m"),
        json!({})
    ));
    // Lines that change the card.
    assert!(flags(
        &mut app,
        "assistant_message_delta",
        Some("a_m"),
        json!({"text": "h"})
    ));
    assert!(flags(
        &mut app,
        "text_completed",
        Some("a_m"),
        json!({"text": "hi"})
    ));
    assert!(flags(&mut app, "reasoning_started", Some("r_1"), json!({})));
    assert!(!flags(
        &mut app,
        "reasoning_started",
        Some("r_1"),
        json!({})
    ));
    assert!(flags(
        &mut app,
        "reasoning_delta",
        Some("r_1"),
        thought.clone()
    ));
    assert!(flags(
        &mut app,
        "reasoning_completed",
        Some("r_1"),
        thought.clone()
    ));
    let raw = json!({"index": 0, "name": "read", "text": "{"});
    assert!(flags(
        &mut app,
        "tool_call_arguments_delta",
        Some("a_n"),
        raw
    ));
    assert!(flags(
        &mut app,
        "assistant_message_completed",
        Some("a_n"),
        json!({})
    ));
    assert!(!flags(
        &mut app,
        "assistant_message_completed",
        Some("a_n"),
        json!({})
    ));
    let read = json!({"name": "read", "arguments": {"path": "a.rs"}});
    assert!(flags(&mut app, "tool_call_requested", Some("a_1"), read));
    // With a group to search, an unknown action still changes nothing.
    assert!(!flags(
        &mut app,
        "tool_call_started",
        Some("a_9"),
        json!({})
    ));
    assert!(!flags(
        &mut app,
        "reasoning_delta",
        Some("r_9"),
        thought.clone()
    ));
    assert!(!flags(
        &mut app,
        "reasoning_completed",
        Some("r_9"),
        thought.clone()
    ));
    assert!(flags(&mut app, "tool_call_started", Some("a_1"), json!({})));
    assert!(flags(
        &mut app,
        "permission_requested",
        Some("a_1"),
        json!({})
    ));
    assert!(flags(&mut app, "tool_call_completed", Some("a_1"), done));
    assert!(flags(
        &mut app,
        "turn_completed",
        None,
        json!({"outcome": "completed"})
    ));
}

#[test]
fn an_interrupt_shows_only_on_the_closing_line_and_its_call_reads_cancelled() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    request(&mut app, "a_1", "shell", json!({"command": "sleep 9"}), 0);
    feed(&mut app, "tool_call_started", Some("a_1"), 0, json!({}));
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_1"),
        2_000,
        json!({"status": "cancelled", "content": []}),
    );
    end(&mut app, "interrupted", 3_000);
    app.open(group(&app));
    let lines = texts(&app);
    let mentions: Vec<_> = lines
        .iter()
        .filter(|line| line.to_lowercase().contains("interrupt"))
        .collect();
    assert_eq!(mentions, ["▣ interrupted · 3s · 1 call"]);
    assert_eq!(last(&app), "▣ interrupted · 3s · 1 call");
    assert!(lines.contains(&"  1 shell sleep 9 · cancelled".to_owned()));
}

/// An `assistant_message_started` for message `a_m`.
fn started(app: &mut App) {
    feed(app, "assistant_message_started", Some("a_m"), 0, json!({}));
}

/// An `assistant_message_completed` for message `a_m`.
fn message_done(app: &mut App, outcome: &str) {
    feed(
        app,
        "assistant_message_completed",
        Some("a_m"),
        0,
        json!({"outcome": outcome}),
    );
}

/// A `retry_scheduled` for message `a_m`. The payload's `attempt` is never
/// read: the card counts the starts. Its `last_attempt` names the last
/// attempt the policy allows, and the row reads it.
fn retry(app: &mut App, attempt: u32, delay_ms: u64, last_attempt: u32) {
    feed(
        app,
        "retry_scheduled",
        Some("a_m"),
        0,
        json!({"code": "rate_limited", "attempt": attempt, "delay_ms": delay_ms,
            "last_attempt": last_attempt}),
    );
}

#[test]
fn a_failed_turn_says_why_then_closes() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "turn_completed",
        None,
        0,
        json!({"outcome": "failed", "error": {"code": "rate_limited",
            "message": "The provider is rate limiting this key.",
            "provider": {"name": "anthropic", "status": 529, "message": "Overloaded"}}}),
    );
    let lines = texts(&app);
    assert_eq!(
        lines[2..],
        [
            "✗ The provider is rate limiting this key. · rate_limited",
            "anthropic said HTTP 529: “Overloaded”",
            "▣ failed",
        ]
    );
    assert!(!dim(&styled(
        &app,
        "✗ The provider is rate limiting this key. · rate_limited"
    )));
    assert!(dim(&styled(&app, "anthropic said HTTP 529: “Overloaded”")));
    // Only a failed login offers "log in".
    assert!(!app.targets().iter().any(|(_, t)| *t == Target::Login));
}

#[test]
fn a_failure_without_a_status_shows_what_the_provider_said() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "turn_completed",
        None,
        0,
        json!({"outcome": "failed", "error": {"code": "credential_failed",
            "message": "acme's credential() failed. Run `fiber login acme`.",
            "provider": {"name": "acme", "message": "init.lua:3: boom"}}}),
    );
    let lines = texts(&app);
    assert_eq!(
        lines[2..],
        [
            "✗ acme's credential() failed. Run `fiber login acme`. · credential_failed",
            "acme said: “init.lua:3: boom”",
            "▣ failed",
        ]
    );
    assert!(dim(&styled(&app, "acme said: “init.lua:3: boom”")));
}

#[test]
fn a_failed_login_offers_log_in_on_its_error_line() {
    let mut app = app();
    start(&mut app, "go", 0);
    feed(
        &mut app,
        "turn_completed",
        None,
        0,
        json!({"outcome": "failed", "error": {"code": "authentication_failed",
            "message": "The key was refused."}}),
    );
    assert_eq!(
        texts(&app)[2..],
        ["✗ The key was refused. · authentication_failed", "▣ failed"]
    );
    assert_eq!(
        target(&app, "✗ The key was refused. · authentication_failed"),
        Target::Login
    );
}

#[test]
fn a_pending_retry_is_a_row_after_the_open_turn() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    retry(&mut app, 9, 3_001, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 4s · rate_limited · attempt 2 of 4"
    );
    started(&mut app);
    retry(&mut app, 9, 4_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 4s · rate_limited · attempt 3 of 4"
    );
    started(&mut app);
    retry(&mut app, 9, 0, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 0s · rate_limited · attempt 4 of 4"
    );
}

#[test]
fn a_retried_request_shows_its_attempt_of_its_last_attempt() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 4_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 4s · rate_limited · attempt 2 of 4"
    );
    insta::assert_snapshot!("retried_request", texts(&app).join("\n"));
}

#[test]
fn a_new_request_counts_its_attempts_from_one() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    started(&mut app);
    message_done(&mut app, "completed");
    text(&mut app, "a_m", "Hi", 0);
    // A request after one that got through starts over.
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 2 of 4"
    );
    // So does the next step's.
    started(&mut app);
    step(&mut app, 0);
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 2 of 4"
    );
}

#[test]
fn attempts_are_counted_from_lines_a_late_attach_replays() {
    // Catching up carries the durable lines and not the `retry_scheduled`.
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    for _ in 0..2 {
        started(&mut app);
        message_done(&mut app, "failed");
    }
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 4 of 4"
    );
}

#[test]
fn a_handoff_note_request_counts_its_own_attempts() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    started(&mut app);
    message_done(&mut app, "failed");
    feed(
        &mut app,
        "handoff_started",
        None,
        0,
        json!({"trigger": "overflow"}),
    );
    started(&mut app);
    message_done(&mut app, "failed");
    retry(&mut app, 9, 1_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 2 of 4"
    );
}

#[test]
fn the_retry_row_reads_its_total_from_the_event() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    retry(&mut app, 9, 1_000, 2);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 2 of 2"
    );
}

#[test]
fn a_cancelled_wait_keeps_the_failed_attempts() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    retry(&mut app, 9, 1_000, 4);
    started(&mut app);
    retry(&mut app, 9, 1_000, 4);
    assert_eq!(
        last(&app),
        "↻ Retrying in 1s · rate_limited · attempt 3 of 4"
    );
    end(&mut app, "interrupted", 1);
    ctrl_o(&mut app);
    assert_eq!(
        texts(&app)[3..5],
        [
            "  1 model call failed · rate_limited · attempt 1",
            "    model call failed · rate_limited · attempt 2",
        ]
    );
}

#[test]
fn the_retry_row_clears_once_the_call_gets_through() {
    let kinds: [(&str, Option<&str>, Value); 9] = [
        (
            "assistant_message_delta",
            Some("a_m"),
            json!({"text": "Hi"}),
        ),
        ("text_completed", Some("a_m"), json!({"text": "Hi"})),
        ("reasoning_started", Some("a_r"), json!({})),
        (
            "tool_call_arguments_delta",
            Some("a_m"),
            json!({"index": 0, "text": "{", "name": "read"}),
        ),
        (
            "tool_call_requested",
            Some("a_1"),
            json!({"name": "read", "arguments": {"path": "a"}}),
        ),
        ("tool_call_started", Some("a_9"), json!({})),
        ("tool_call_delta", Some("a_9"), json!({"text": "x"})),
        (
            "tool_call_completed",
            Some("a_9"),
            json!({"status": "completed", "content": []}),
        ),
        ("turn_completed", None, json!({"outcome": "completed"})),
    ];
    for (kind, action, payload) in kinds {
        let mut app = app();
        start(&mut app, "go", 0);
        step(&mut app, 0);
        retry(&mut app, 2, 1_000, 4);
        feed(&mut app, kind, action, 0, payload);
        assert!(
            !texts(&app).iter().any(|line| line.starts_with('↻')),
            "{kind}"
        );
    }
    // Other lines leave it.
    let mut app = app();
    start(&mut app, "go", 0);
    started(&mut app);
    retry(&mut app, 2, 1_000, 4);
    step(&mut app, 0);
    assert!(last(&app).starts_with('↻'));
}

#[test]
fn a_failed_model_call_is_counted_on_the_summary_and_in_the_ledger() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    retry(&mut app, 2, 1_000, 4);
    message_done(&mut app, "completed");
    call(&mut app, "a_1", "read", json!({"path": "a.rs"}), 0);
    started(&mut app);
    retry(&mut app, 3, 1_000, 4);
    text(&mut app, "a_m", "Done.", 0);
    assert_eq!(texts(&app)[2], "• Read 1 file · 2 failed model calls");
    app.open(group(&app));
    assert_eq!(
        texts(&app)[3..6],
        [
            "  1 model call failed · rate_limited · attempt 1",
            "    model call failed · rate_limited · attempt 1",
            "    read a.rs",
        ]
    );
}

#[test]
fn a_group_with_only_a_failed_call_still_draws_its_summary() {
    let mut app = app();
    start(&mut app, "go", 0);
    step(&mut app, 0);
    started(&mut app);
    retry(&mut app, 2, 1_000, 4);
    text(&mut app, "a_m", "Done.", 0);
    assert_eq!(
        texts(&app),
        [" go ", "00:00", "• 1 failed model call", "Done."]
    );
    // Ctrl+O counts it as a ledger.
    ctrl_o(&mut app);
    assert_eq!(
        texts(&app)[3],
        "  1 model call failed · rate_limited · attempt 1"
    );
}

/// An `mcp_server_failed` line for `server`.
fn mcp_failed(app: &mut App, server: &str) {
    feed(
        app,
        "mcp_server_failed",
        None,
        0,
        json!({"server": server, "reason": "died", "will_restart": true,
            "error": {"code": "mcp_server_unavailable",
                "message": format!("The MCP server {server} stopped.")}}),
    );
}

#[test]
fn a_failed_mcp_server_is_a_warning_line_in_or_out_of_a_turn() {
    let mut app = app();
    mcp_failed(&mut app, "one");
    start(&mut app, "go", 0);
    mcp_failed(&mut app, "two");
    text(&mut app, "a_m", "Hi", 0);
    end(&mut app, "completed", 0);
    mcp_failed(&mut app, "three");
    feed(
        &mut app,
        "mcp_server_ready",
        None,
        0,
        json!({"server": "three"}),
    );
    assert_eq!(
        texts(&app),
        [
            "⚠ The MCP server one stopped.",
            " go ",
            "00:00",
            "⚠ The MCP server two stopped.",
            "Hi",
            "▣ completed",
            "⚠ The MCP server three stopped.",
        ]
    );
}

#[test]
fn a_part_turn_keeps_the_step_it_reached() {
    // A page that begins inside a running turn folds from its step on: new
    // items join that step's section, not the first.
    let mut fold = super::Fold::default();
    let mut turn = super::Turn::part(3);
    assert!(turn.reasoning_started("a_r", 7, &mut fold));
    let steps: Vec<u64> = turn
        .groups()
        .flat_map(|group| group.sections.iter().map(|section| section.step))
        .collect();
    assert_eq!(steps, vec![3]);
}

#[test]
fn a_thoughts_toggle_target_is_its_actions_id() {
    // Without its id the thought keeps the default zero, and its toggle
    // target names nothing.
    let id = super::target_id("a_t");
    assert_ne!(id, 0);
    let mut fold = super::Fold::default();
    let mut turn = super::Turn::new(vec!["hi".to_owned()], 0);
    assert!(turn.reasoning_started("a_t", 1, &mut fold));
    assert_eq!(
        turn.groups()
            .flat_map(|group| group.thoughts())
            .next()
            .map(|thought| thought.id),
        Some(id)
    );
    // The matching target opens it; another id opens nothing. The turn
    // holds no asides, so only its groups answer.
    assert!(turn.set_open(&Target::Thought(id), true));
    assert!(
        turn.groups()
            .flat_map(|group| group.thoughts())
            .next()
            .is_some_and(|thought| thought.open)
    );
    assert!(!turn.set_open(&Target::Thought(id.wrapping_add(1)), false));
}

#[test]
fn group_and_call_targets_name_only_their_item() {
    // `0` names nothing: every id below is a hashed action, never zero.
    let key_id = super::target_id("a_k");
    let thought_id = super::target_id("a_t");
    let call_id = super::target_id("a_c");
    assert_ne!(key_id, 0);
    assert_ne!(thought_id, 0);
    assert_ne!(call_id, 0);
    let mut group = super::Group {
        key: Some("a_k".to_owned()),
        sections: vec![super::Section {
            step: 0,
            thoughts: vec![super::Thought {
                id: thought_id,
                action: "a_t".to_owned(),
                ..Default::default()
            }],
            calls: vec![super::Call {
                id: call_id,
                action: "a_c".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(group.set_open(&Target::Group(key_id), true));
    assert!(group.open);
    assert!(!group.set_open(&Target::Group(0), false));
    assert!(group.set_open(&Target::Thought(thought_id), true));
    assert!(group.thoughts().next().is_some_and(|thought| thought.open));
    assert!(!group.set_open(&Target::Thought(0), false));
    assert!(group.set_open(&Target::Call(call_id), true));
    assert!(group.calls().next().is_some_and(|call| call.open));
    assert!(!group.set_open(&Target::Call(0), false));
    assert!(!group.set_open(&Target::Login, false));
}

#[test]
fn turn_set_open_finds_an_aside_with_no_groups() {
    // No group answers, so only the aside can: `&&` for `||` fails here.
    let mut turn = super::Turn::new(Vec::new(), 0);
    turn.entries
        .push(super::Entry::Aside(super::crash::Aside::Orphans {
            id: 7,
            jobs: vec![("job".to_owned(), "lost".to_owned())],
            open: false,
        }));
    assert!(turn.set_open(&Target::Orphans(7), true));
    assert!(!turn.set_open(&Target::Orphans(8), false));
    assert!(!turn.set_open(&Target::Group(0), false));
}
