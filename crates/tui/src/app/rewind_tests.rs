//! Tests for `/rewind` (#680): the view listing the conversation's turns
//! back through the `forked_from` chain, the step boundaries each turn
//! opens into, and the rewind the person sends from the chosen point.

use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;
use contract::{ActionId, Envelope, SessionId, TurnId};
use serde_json::{Value, json};

use crate::app::Effect;
use crate::app::commands::commands_testkit::{SESSION, attached, enter, sent, session_line};

const OTHER: &str = "s_bbbbbbbbbbbbbbbb";

/// One durable history line, as a `history` answer carries it.
fn history_line(
    session: &str,
    kind: &str,
    seq: u64,
    turn: Option<&str>,
    action: Option<&str>,
    payload: Value,
) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: turn.map(|id| TurnId(id.to_owned())),
        action_id: action.map(|id| ActionId(id.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A `turn_started` input carrying one driver message.
fn message_input(text: &str) -> Value {
    json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": text}]}]})
}

/// A `turn_started` input for the summary turn a `rewind` asked for.
fn summary_input() -> Value {
    json!({"input": [{"type": "rewind", "command_id": "c_9"}]})
}

/// A `session_started` payload, forked from `parent` at `seq` when given.
fn started(parent: Option<(&str, u64)>) -> Value {
    match parent {
        Some((session, seq)) => {
            json!({"forked_from": {"session_id": session, "seq": seq}})
        }
        None => json!({}),
    }
}

/// The attached session's first history answer: A is a rewind of B at
/// B's seq 3, holding two turns of its own.
fn segment_a() -> Vec<Envelope> {
    vec![
        history_line(
            SESSION,
            "session_started",
            0,
            None,
            None,
            started(Some((OTHER, 3))),
        ),
        history_line(
            SESSION,
            "turn_started",
            1,
            Some("t_1"),
            None,
            message_input("first"),
        ),
        history_line(SESSION, "step_started", 2, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            3,
            Some("t_1"),
            None,
            json!({"outcome": "completed"}),
        ),
        history_line(
            SESSION,
            "turn_started",
            4,
            Some("t_2"),
            None,
            message_input("second"),
        ),
        history_line(SESSION, "step_started", 5, Some("t_2"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            6,
            Some("t_2"),
            None,
            json!({"outcome": "completed"}),
        ),
    ]
}

/// B's lines up to the point A continues from: one turn, then the root.
fn segment_b() -> Vec<Envelope> {
    vec![
        history_line(OTHER, "session_started", 0, None, None, started(None)),
        history_line(
            OTHER,
            "turn_started",
            1,
            Some("t_0"),
            None,
            message_input("older"),
        ),
        history_line(
            OTHER,
            "turn_completed",
            2,
            Some("t_0"),
            None,
            json!({"outcome": "completed"}),
        ),
    ]
}

/// Feeds a `command_accepted` history answer for `id`, returning what the
/// app sends next.
fn accept(app: &mut crate::app::App, id: &Value, lines: Vec<Envelope>) -> Vec<String> {
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id": id, "result": {"lines": lines}}),
    ))
}

/// Opens `/rewind` at the fake clock's now, returning the command lines.
fn open(app: &mut crate::app::App, clock: &Arc<fakes::clock::FakeClock>) -> Vec<Value> {
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
    let lines = sent(enter(app, "/rewind"));
    assert_eq!(app.draft(), "");
    assert!(app.session_view_screen(80).is_some());
    lines
}

/// The rewind rows' text.
fn rows(app: &crate::app::App) -> Vec<String> {
    app.session_view_screen(80)
        .expect("the view is open")
        .rows
        .iter()
        .map(|row| row.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect()
}

/// The selected point's holding session and seq, as the sent `rewind`
/// would name them: the selected row's first boundary.
fn point(app: &crate::app::App) -> Option<(String, u64)> {
    let frame = app.session_view_screen(80)?;
    let at = frame.list.selected();
    app.rewind_turn_bounds(at).first().cloned()
}

#[test]
fn rewind_asks_history_on_open() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "history");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({}));
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Rewind");
    assert_eq!(frame.command, "/rewind");
    assert!(frame.rows.is_empty());
}

#[test]
fn two_segments_fold_with_the_default_on_the_latest_turn() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    // The first answer folds A's turns and pages B next, naming its
    // session and stopping at the point A continues from: one request
    // on the wire.
    let next = accept(&mut app, &lines[0]["id"], segment_a());
    assert_eq!(rows(&app), ["before: first", "before: second"]);
    assert_eq!(next.len(), 1);
    let asked: Value = serde_json::from_str(&next[0]).unwrap_or_default();
    assert_eq!(asked["command"], "history");
    assert_eq!(asked["session_id"], SESSION);
    assert_eq!(asked["args"], json!({"session_id": OTHER, "to_seq": 3}));
    // B's answer folds its turn before A's, and the walk ends at the
    // root: nothing more goes out.
    let id = asked["id"].clone();
    let done = accept(&mut app, &id, segment_b());
    assert!(done.is_empty());
    assert_eq!(
        rows(&app),
        ["before: older", "before: first", "before: second"]
    );
    // The default is the start of the latest turn: turn-start seq minus
    // one, distinct from the after-input boundary at turn-start seq.
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.list.selected(), 2);
    assert_eq!(point(&app), Some((SESSION.to_owned(), 3)));
    let bounds = app.rewind_turn_bounds(2);
    assert_eq!(
        bounds,
        vec![
            (SESSION.to_owned(), 3),
            (SESSION.to_owned(), 4),
            (SESSION.to_owned(), 6),
        ]
    );
}

#[test]
fn a_summary_turn_lists_but_does_not_take_the_default() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let held = vec![
        history_line(SESSION, "session_started", 0, None, None, started(None)),
        history_line(
            SESSION,
            "turn_started",
            1,
            Some("t_1"),
            None,
            message_input("first"),
        ),
        history_line(
            SESSION,
            "turn_completed",
            2,
            Some("t_1"),
            None,
            json!({"outcome": "completed"}),
        ),
        history_line(
            SESSION,
            "turn_started",
            3,
            Some("t_2"),
            None,
            summary_input(),
        ),
        history_line(SESSION, "step_started", 4, Some("t_2"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            5,
            Some("t_2"),
            None,
            json!({"outcome": "completed"}),
        ),
        history_line(
            SESSION,
            "turn_started",
            6,
            Some("t_3"),
            None,
            message_input("third"),
        ),
        history_line(SESSION, "step_started", 7, Some("t_3"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            8,
            Some("t_3"),
            None,
            json!({"outcome": "completed"}),
        ),
    ];
    let done = accept(&mut app, &lines[0]["id"], held);
    assert!(done.is_empty());
    assert_eq!(
        rows(&app),
        ["before: first", "before: rewind summary", "before: third"]
    );
    // The default is the latest turn that is not a summary's, while the
    // summary turn still lists its step boundaries.
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.list.selected(), 2);
    assert_eq!(point(&app), Some((SESSION.to_owned(), 5)));
    assert_eq!(
        app.rewind_turn_bounds(1),
        vec![
            (SESSION.to_owned(), 2),
            (SESSION.to_owned(), 3),
            (SESSION.to_owned(), 5),
        ]
    );
}

#[test]
fn an_empty_log_lists_no_turns() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    // No lines at all, and a log with no turn in it, both list nothing.
    for held in [
        Vec::new(),
        vec![history_line(
            SESSION,
            "session_started",
            0,
            None,
            None,
            started(None),
        )],
    ] {
        let done = accept(&mut app, &lines[0]["id"], held);
        assert!(done.is_empty());
        assert!(rows(&app).is_empty());
        let frame = app.session_view_screen(80).expect("the view is open");
        assert_eq!(frame.below, ["No turns to rewind."]);
    }
}

#[test]
fn post_point_lines_refuse_the_whole_segment() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let next = accept(&mut app, &lines[0]["id"], segment_a());
    assert_eq!(next.len(), 1);
    // B's answer reaches past the point A continues from: the segment
    // is refused, so the cut-off turn never lists and the walk ends.
    let mut past = segment_b();
    past.push(history_line(
        OTHER,
        "turn_started",
        4,
        Some("t_cut"),
        None,
        message_input("cut off"),
    ));
    let asked: Value = serde_json::from_str(&next[0]).unwrap_or_default();
    let done = accept(&mut app, &asked["id"], past);
    assert!(done.is_empty());
    assert_eq!(rows(&app), ["before: first", "before: second"]);
}

#[test]
fn the_point_itself_belongs_to_the_parent() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let next = accept(&mut app, &lines[0]["id"], segment_a());
    // B's answer ends exactly at the point A continues from: the line
    // counts in B's log, so the segment folds and the walk ends with no
    // further request.
    let at = vec![
        history_line(OTHER, "session_started", 0, None, None, started(None)),
        history_line(
            OTHER,
            "turn_started",
            1,
            Some("t_0"),
            None,
            message_input("older"),
        ),
        history_line(OTHER, "step_started", 2, Some("t_0"), None, json!({})),
        history_line(
            OTHER,
            "turn_completed",
            3,
            Some("t_0"),
            None,
            json!({"outcome": "completed"}),
        ),
    ];
    let asked: Value = serde_json::from_str(&next[0]).unwrap_or_default();
    let done = accept(&mut app, &asked["id"], at);
    assert!(done.is_empty());
    assert_eq!(
        rows(&app),
        ["before: older", "before: first", "before: second"]
    );
    assert_eq!(
        app.rewind_turn_bounds(0),
        vec![
            (OTHER.to_owned(), 0),
            (OTHER.to_owned(), 1),
            (OTHER.to_owned(), 3),
        ]
    );
}

#[test]
fn an_unreadable_answer_ends_the_walk_with_the_error() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    // The answer carries no lines: the walk ends with the error, and a
    // late answer for the dropped id folds nowhere.
    let done = app.on_line(session_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {}}),
    ));
    assert!(done.is_empty());
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.below, ["the answer could not be read"]);
    accept(&mut app, &lines[0]["id"], segment_a());
    assert!(rows(&app).is_empty());
}

#[test]
fn a_gapped_answer_ends_the_walk() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    // The first answer holds the two latest lines; the older answer
    // skips seq 4, so it leaves a gap, folds nowhere, and ends the walk
    // with the turns read so far.
    let mut first = segment_a();
    first.drain(..5);
    let next = accept(&mut app, &lines[0]["id"], first);
    assert_eq!(next.len(), 1);
    // The next page stops just below the lines read so far.
    let asked: Value = serde_json::from_str(&next[0]).unwrap_or_default();
    assert_eq!(asked["command"], "history");
    assert_eq!(asked["session_id"], SESSION);
    assert_eq!(asked["args"], json!({"to_seq": 4}));
    let mut older = segment_a();
    older.truncate(4);
    older.remove(1);
    let done = accept(&mut app, &asked["id"], older);
    assert!(done.is_empty());
    assert!(rows(&app).is_empty());
}

#[test]
fn unanswered_calls_drop_later_boundaries() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    // The call starting at seq 3 never answers: the start and the
    // after-input boundary stand, while the boundaries at and past the
    // call do not.
    let held = vec![
        history_line(SESSION, "session_started", 0, None, None, started(None)),
        history_line(
            SESSION,
            "turn_started",
            1,
            Some("t_1"),
            None,
            message_input("first"),
        ),
        history_line(SESSION, "step_started", 2, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "tool_call_started",
            3,
            Some("t_1"),
            Some("a_1"),
            json!({"effects": [], "reversible": true}),
        ),
        history_line(SESSION, "step_started", 4, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            5,
            Some("t_1"),
            None,
            json!({"outcome": "completed"}),
        ),
    ];
    let done = accept(&mut app, &lines[0]["id"], held);
    assert!(done.is_empty());
    assert_eq!(
        app.rewind_turn_bounds(0),
        vec![(SESSION.to_owned(), 0), (SESSION.to_owned(), 1)]
    );

    // An answered call keeps its boundaries: the one at its completion
    // and the ones past it.
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let held = vec![
        history_line(SESSION, "session_started", 0, None, None, started(None)),
        history_line(
            SESSION,
            "turn_started",
            1,
            Some("t_1"),
            None,
            message_input("first"),
        ),
        history_line(SESSION, "step_started", 2, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "tool_call_started",
            3,
            Some("t_1"),
            Some("a_1"),
            json!({"effects": [], "reversible": true}),
        ),
        history_line(
            SESSION,
            "tool_call_completed",
            4,
            Some("t_1"),
            Some("a_1"),
            json!({"status": "completed", "content": []}),
        ),
        history_line(SESSION, "step_started", 5, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            6,
            Some("t_1"),
            None,
            json!({"outcome": "completed"}),
        ),
    ];
    let done = accept(&mut app, &lines[0]["id"], held);
    assert!(done.is_empty());
    assert_eq!(
        app.rewind_turn_bounds(0),
        vec![
            (SESSION.to_owned(), 0),
            (SESSION.to_owned(), 1),
            (SESSION.to_owned(), 4),
            (SESSION.to_owned(), 6),
        ]
    );
}

#[test]
fn history_wait_times_out_as_a_lost_connection() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let sent_at = clock.now();
    // The wait arms the frame's wake at its deadline: the signal the
    // test advances past.
    assert_eq!(app.take_wake(), Some(sent_at + Duration::from_secs(10)));
    clock.advance(Duration::from_secs(11));
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
    assert!(app.rewind_due(clock.now()));
    // Past the bound the wait ends as a lost connection, and the late
    // answer finds no pending id, so it folds nowhere.
    app.disconnected();
    assert_eq!(app.notice(), Some("Connection lost."));
    assert_eq!(app.take_wake(), None);
    accept(&mut app, &lines[0]["id"], segment_a());
    assert!(rows(&app).is_empty());
}

#[test]
fn the_history_wait_ends_exactly_at_ten_seconds() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    open(&mut app, &clock);
    let sent_at = clock.now();
    assert!(!app.rewind_due(sent_at + Duration::from_secs(9)));
    let almost = (sent_at + Duration::from_secs(10))
        .checked_sub(Duration::from_nanos(1))
        .expect("one nanosecond in");
    assert!(!app.rewind_due(almost));
    assert!(app.rewind_due(sent_at + Duration::from_secs(10)));
    assert!(app.rewind_due(sent_at + Duration::from_secs(11)));
}

#[test]
fn stream_reopened_ends_the_wait_at_once() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "stream_reopened".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    assert_eq!(app.take_wake(), None);
    // The late answer matches no pending id and folds nowhere.
    accept(&mut app, &lines[0]["id"], segment_a());
    assert!(rows(&app).is_empty());
}

#[test]
fn close_and_reopen_drops_the_old_answer() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let first = open(&mut app, &clock);
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_none());
    let second = open(&mut app, &clock);
    assert_ne!(first[0]["id"], second[0]["id"]);
    // The old answer arrives after the reopen: it folds nowhere.
    accept(&mut app, &first[0]["id"], segment_a());
    assert!(rows(&app).is_empty());
    // The new answer folds.
    accept(&mut app, &second[0]["id"], segment_a());
    assert_eq!(rows(&app), ["before: first", "before: second"]);
}

#[test]
fn a_rejected_history_keeps_the_error() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    app.on_line(session_line(
        "command_rejected",
        json!({"command_id": lines[0]["id"], "code": "invalid_arguments",
            "message": "from_session_id is off the chain"}),
    ));
    assert_eq!(app.take_wake(), None);
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.below, ["from_session_id is off the chain"]);
    // A late answer for the dropped id folds nowhere.
    accept(&mut app, &lines[0]["id"], segment_a());
    assert!(rows(&app).is_empty());
}
