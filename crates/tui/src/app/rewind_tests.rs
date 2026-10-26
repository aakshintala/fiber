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

/// The summary checkbox row, unchecked.
const CHECK: &str = "[ ] Summarise the path after the point";
/// The summary checkbox row, checked.
const CHECKED: &str = "[x] Summarise the path after the point";
/// The Rewind row.
const ACTION: &str = "Rewind";
/// The session an accepted `rewind` names.
const NEW: &str = "s_cccccccccccccccc";
/// A `summary_failed` refusal's first choice: try the summary again.
const RETRY: &str = "Retry with summary";
/// A `summary_failed` refusal's second choice: rewind with no summary.
const WITHOUT: &str = "Rewind without summary";
/// A `summary_failed` refusal's third choice: back to the list.
const CANCEL: &str = "Cancel";

/// A hub `command_rejected` for `id` with `code` and `message`.
fn hub_refused(id: &Value, code: &str, message: &str) -> crate::link::Line {
    crate::link::Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), id.clone()),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `stream_closed` naming `session`.
fn hub_closed(session: &str) -> crate::link::Line {
    crate::link::Line::Hub(contract::HubLine {
        kind: "stream_closed".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [("session_id".to_owned(), Value::String(session.to_owned()))]
            .into_iter()
            .collect(),
    })
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(
        rows(&app),
        ["before: first", "before: second", CHECK, ACTION]
    );
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
        [
            "before: older",
            "before: first",
            "before: second",
            CHECK,
            ACTION
        ]
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
        [
            "before: first",
            "before: rewind summary",
            "before: third",
            CHECK,
            ACTION
        ]
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
        assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(
        rows(&app),
        ["before: first", "before: second", CHECK, ACTION]
    );
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
        [
            "before: older",
            "before: first",
            "before: second",
            CHECK,
            ACTION
        ]
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
    // The new answer folds.
    accept(&mut app, &second[0]["id"], segment_a());
    assert_eq!(
        rows(&app),
        ["before: first", "before: second", CHECK, ACTION]
    );
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
    assert_eq!(rows(&app), [CHECK, ACTION]);
}

/// Moves the list cursor onto visible row `at`, through the public keys.
fn goto(app: &mut crate::app::App, clock: &Arc<fakes::clock::FakeClock>, at: usize) {
    for _ in 0..64 {
        let selected = app
            .session_view_screen(80)
            .expect("the view is open")
            .list
            .selected();
        if selected == at {
            return;
        }
        let key = if selected < at {
            crate::keys::Key::Down
        } else {
            crate::keys::Key::Up
        };
        app.on_key(key, clock.now());
    }
    panic!("never reached row {at}");
}

/// Sends the rewind from the Rewind row, returning the command lines.
fn send_rewind(app: &mut crate::app::App, clock: &Arc<fakes::clock::FakeClock>) -> Vec<Value> {
    let at = app
        .session_view_screen(80)
        .expect("the view is open")
        .rows
        .len()
        - 1;
    goto(app, clock, at);
    sent(app.on_key(crate::keys::Key::Enter, clock.now()))
}

/// Refuses the sent rewind with `code`, returning the view's rows.
fn refuse(app: &mut crate::app::App, id: &Value, code: &str) {
    app.on_line(session_line(
        "command_rejected",
        json!({"command_id": id, "code": code, "message": format!("{code} happened")}),
    ));
}

/// One turn holding a writing call, a shell call, and a step's text.
fn calls_segment() -> Vec<Envelope> {
    vec![
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
            "tool_call_requested",
            3,
            Some("t_1"),
            Some("a_w"),
            json!({"name": "write", "arguments": {"path": "/w/a.rs"}}),
        ),
        history_line(
            SESSION,
            "tool_call_started",
            4,
            Some("t_1"),
            Some("a_w"),
            json!({"effects": ["writes"], "paths": ["/w/a.rs"], "reversible": true}),
        ),
        history_line(
            SESSION,
            "tool_call_completed",
            5,
            Some("t_1"),
            Some("a_w"),
            json!({"status": "completed", "content": []}),
        ),
        history_line(
            SESSION,
            "tool_call_requested",
            6,
            Some("t_1"),
            Some("a_s"),
            json!({"name": "shell", "arguments": {"command": "make"}}),
        ),
        history_line(
            SESSION,
            "tool_call_started",
            7,
            Some("t_1"),
            Some("a_s"),
            json!({"effects": ["executes"], "reversible": false}),
        ),
        history_line(
            SESSION,
            "tool_call_completed",
            8,
            Some("t_1"),
            Some("a_s"),
            json!({"status": "completed", "content": []}),
        ),
        history_line(
            SESSION,
            "text_completed",
            9,
            Some("t_1"),
            Some("a_m"),
            json!({"text": "did things"}),
        ),
        history_line(SESSION, "step_started", 10, Some("t_1"), None, json!({})),
        history_line(
            SESSION,
            "turn_completed",
            11,
            Some("t_1"),
            None,
            json!({"outcome": "completed"}),
        ),
    ]
}

/// Folds one running job and one running delegate, neither completed.
fn run_jobs(app: &mut crate::app::App) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task one", "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_2", "description": "delegate work", "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_2", "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// The view's detail lines under the rows.
fn below(app: &crate::app::App) -> Vec<String> {
    app.session_view_screen(80).expect("the view is open").below
}

#[test]
fn enter_opens_the_selected_turn_into_its_boundaries() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    // Enter opens the selected turn: its step boundaries draw under it.
    app.on_key(crate::keys::Key::Enter, clock.now());
    assert_eq!(
        rows(&app),
        [
            "before: first",
            "before: second",
            "  start of turn",
            "  after input",
            "  end of turn",
            CHECK,
            ACTION,
        ]
    );
    // Enter again closes it back to the turn rows, sending nothing:
    // the turn row opens, it never sends.
    assert_eq!(
        app.on_key(crate::keys::Key::Enter, clock.now()),
        Effect::None
    );
    assert_eq!(
        rows(&app),
        ["before: first", "before: second", CHECK, ACTION]
    );
    // Enter on a boundary row sends from that point at once.
    app.on_key(crate::keys::Key::Enter, clock.now());
    goto(&mut app, &clock, 3);
    let out = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "rewind");
    assert_eq!(out[0]["session_id"], SESSION);
    assert_eq!(out[0]["args"], json!({"seq": 4, "summarise": false}));
}

#[test]
fn the_send_names_an_ancestor_holding_session() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    let next = accept(&mut app, &lines[0]["id"], segment_a());
    let asked: Value = serde_json::from_str(&next[0]).unwrap_or_default();
    accept(&mut app, &asked["id"], segment_b());
    // The default point is the latest turn's start on the attached
    // session: open the ancestor's turn and send from its start
    // boundary. Enter on the boundary sends at once, since reaching the
    // Rewind row would cross content rows and move the point.
    goto(&mut app, &clock, 0);
    app.on_key(crate::keys::Key::Enter, clock.now());
    goto(&mut app, &clock, 1);
    let out = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "rewind");
    assert_eq!(out[0]["session_id"], SESSION);
    assert_eq!(
        out[0]["args"],
        json!({"from_session_id": OTHER, "seq": 0, "summarise": false})
    );
}

#[test]
fn the_point_past_the_calls_details_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], calls_segment());
    app.on_key(crate::keys::Key::Enter, clock.now());
    // The turn's end sits past every call: the detail empties.
    goto(&mut app, &clock, 4);
    assert_eq!(
        below(&app),
        [
            "Files written since the point: none",
            "Calls that may have changed files: none",
            "None of it is undone.",
            "Jobs that stop: none",
        ]
    );
    // The start sits before every call: everything lists.
    goto(&mut app, &clock, 1);
    assert_eq!(
        below(&app),
        [
            "Files written since the point: /w/a.rs",
            "Calls that may have changed files: shell",
            "None of it is undone.",
            "Jobs that stop: none",
        ]
    );
}

#[test]
fn the_detail_names_files_calls_jobs_and_no_restore() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    run_jobs(&mut app);
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], calls_segment());
    // The default point is the turn's start: everything after it lists.
    app.on_key(crate::keys::Key::Enter, clock.now());
    assert_eq!(
        rows(&app),
        [
            "before: first",
            "  start of turn",
            "  after input",
            "  after step 1 · did things",
            "  end of turn",
            CHECK,
            ACTION,
        ]
    );
    assert_eq!(
        below(&app),
        [
            "Files written since the point: /w/a.rs",
            "Calls that may have changed files: shell",
            "None of it is undone.",
            "Jobs that stop: task one, delegate work",
        ]
    );
    // Past the calls the detail empties, while the delegate still lists
    // with no marker of its own.
    goto(&mut app, &clock, 4);
    assert_eq!(
        below(&app),
        [
            "Files written since the point: none",
            "Calls that may have changed files: none",
            "None of it is undone.",
            "Jobs that stop: task one, delegate work",
        ]
    );
}

#[test]
fn space_toggles_the_summary_checkbox() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    assert!(rows(&app).contains(&CHECK.to_owned()));
    // Space toggles wherever the cursor sits.
    app.on_key(crate::keys::Key::Char(' '), clock.now());
    assert!(rows(&app).contains(&CHECKED.to_owned()));
    // Enter on the checkbox row toggles it back.
    let check = rows(&app).len() - 2;
    goto(&mut app, &clock, check);
    app.on_key(crate::keys::Key::Enter, clock.now());
    assert!(rows(&app).contains(&CHECK.to_owned()));
}

#[test]
fn each_refusal_replaces_the_rewind_button() {
    for code in [
        "busy",
        "not_step_boundary",
        "delegate_session",
        "invalid_arguments",
        "session_held",
    ] {
        let clock = fakes::clock::FakeClock::new();
        let mut app = attached();
        let lines = open(&mut app, &clock);
        accept(&mut app, &lines[0]["id"], segment_a());
        let out = send_rewind(&mut app, &clock);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["command"], "rewind");
        refuse(&mut app, &out[0]["id"], code);
        // The refusal's text stands in place of the button, the view
        // stays open, and the error row sends nothing more.
        let grown = rows(&app);
        let want = format!("{code} happened");
        assert_eq!(grown.last(), Some(&want));
        assert!(app.session_view_screen(80).is_some());
        let again = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
        assert!(again.is_empty());
    }
}

#[test]
fn esc_closes_only_when_no_send_is_pending() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    assert_eq!(out.len(), 1);
    // Esc while the rewind is pending is ignored: the view stays open.
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_some());
    // Once the refusal settles the send, Esc closes the view.
    refuse(&mut app, &out[0]["id"], "busy");
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_none());
}

#[test]
fn one_rewind_at_a_time() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    assert_eq!(out.len(), 1);
    // A second Enter on the sending row sends nothing more.
    let again = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert!(again.is_empty());
}

#[test]
fn rewind_is_disabled_with_no_turns() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    open(&mut app, &clock);
    let frame = app.session_view_screen(80).expect("the view is open");
    let action = frame.rows.last().expect("the Rewind row");
    assert_eq!(action[0].0, "Rewind");
    assert_eq!(action[0].2, crate::swapped::Ink::Muted);
    // Enter on the disabled row sends nothing.
    goto(&mut app, &clock, frame.rows.len() - 1);
    let lines = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert!(lines.is_empty());
}

#[test]
fn clicks_select_toggle_and_close() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    // A click selects the row: the default sat on the second turn.
    app.session_view_click(crate::swapped::Spot::Row(0));
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.list.selected(), 0);
    // A click on the checkbox cell toggles it.
    let at = frame.rows.len() - 2;
    app.session_view_click(crate::swapped::Spot::Cell(at, 0));
    assert!(rows(&app).contains(&CHECKED.to_owned()));
    // The close target closes the view.
    app.session_view_click(crate::swapped::Spot::Close);
    assert!(app.session_view_screen(80).is_none());
}

#[test]
fn the_opened_view_snapshot() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    run_jobs(&mut app);
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], calls_segment());
    app.on_key(crate::keys::Key::Enter, clock.now());
    insta::assert_snapshot!("rewind_opened_80x24", shot(&app));
}

#[test]
fn the_refused_view_snapshot() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    refuse(&mut app, &out[0]["id"], "session_held");
    insta::assert_snapshot!("rewind_refused_80x24", shot(&app));
}

#[test]
fn the_send_line_names_the_point_exactly() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    assert_eq!(out.len(), 1);
    let id = out[0]["id"].clone();
    // The whole line on the wire: the attached session, the point's
    // seq, and no holding session of its own.
    assert_eq!(
        out[0],
        json!({
            "id": id,
            "command": "rewind",
            "session_id": SESSION,
            "args": {"seq": 3, "summarise": false},
        })
    );
}

#[test]
fn accepted_rewind_attaches_subscribes_and_closes_the_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    let answered = app.on_line(session_line(
        "command_accepted",
        json!({"command_id": out[0]["id"], "result": {"new_session_id": NEW}}),
    ));
    let answered: Vec<Value> = answered
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The new session is subscribed full and asked its commands.
    assert_eq!(answered.len(), 2);
    assert!(answered.iter().any(|line| line["command"] == "subscribe"
        && line["session_id"] == NEW
        && line["args"] == json!({"level": "full"})));
    assert!(
        answered
            .iter()
            .any(|line| line["command"] == "commands" && line["session_id"] == NEW)
    );
    // The terminal attaches to the new session; the view closes.
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(NEW));
    assert!(app.session_view_screen(80).is_none());
}

#[test]
fn an_accept_for_another_command_or_shape_keeps_waiting() {
    for accept_result in [
        // Another command's accept carrying a new session.
        (json!("c_other"), json!({"new_session_id": NEW})),
        // The sent rewind answered with another command's shape.
        (json!("c_send"), json!({"commands": []})),
    ] {
        let clock = fakes::clock::FakeClock::new();
        let mut app = attached();
        let lines = open(&mut app, &clock);
        accept(&mut app, &lines[0]["id"], segment_a());
        let out = send_rewind(&mut app, &clock);
        let id = if accept_result.0 == "c_send" {
            out[0]["id"].clone()
        } else {
            accept_result.0.clone()
        };
        let answered = app.on_line(session_line(
            "command_accepted",
            json!({"command_id": id, "result": accept_result.1}),
        ));
        assert!(answered.is_empty());
        // No attach, no close: the send still waits, so Esc is ignored.
        assert_eq!(
            app.session().map(|session| session.0.as_str()),
            Some(SESSION)
        );
        assert!(app.session_view_screen(80).is_some());
        assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
        assert!(app.session_view_screen(80).is_some());
    }
}

#[test]
fn a_hub_relayed_rejection_settles_with_the_start_code() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    // The failed new-session start arrives relayed by the hub.
    let answered = app.on_line(hub_refused(&out[0]["id"], "busy", "start failed"));
    assert!(answered.is_empty());
    // The start's code stands in place of the button, the terminal
    // never moves, and the settled send lets Esc close the view.
    assert_eq!(rows(&app).last(), Some(&"start failed".to_owned()));
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(app.session_view_screen(80).is_some());
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_none());
}

#[test]
fn a_hub_rejection_for_another_command_leaves_the_send_pending() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    app.on_line(hub_refused(&json!("c_other"), "busy", "start failed"));
    // The rejection names another command: the rows still show the
    // sending state, and Esc stays ignored while the rewind waits.
    let _ = out;
    assert_eq!(rows(&app).last(), Some(&"Sending…".to_owned()));
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_some());
}

#[test]
fn summary_failed_offers_retry_without_summary_and_cancel() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    let first = out[0]["id"].clone();
    refuse(&mut app, &out[0]["id"], "summary_failed");
    // The reason stands with its three choices under it.
    let grown = rows(&app);
    assert_eq!(
        grown[grown.len() - 4..],
        [
            "summary_failed happened".to_owned(),
            RETRY.to_owned(),
            WITHOUT.to_owned(),
            CANCEL.to_owned(),
        ]
    );
    // Retry sends a fresh rewind with the summary on, whatever the
    // checkbox says: it is off here.
    goto(&mut app, &clock, grown.len() - 3);
    let out = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_ne!(out[0]["id"], first);
    assert_eq!(
        out[0]["args"],
        json!({"seq": 3, "summarise": true}),
        "retry keeps the summary on"
    );
    // Esc stays ignored while the fresh rewind waits.
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    // Rewind without summary sends a fresh rewind with it off, even
    // with the checkbox on.
    refuse(&mut app, &out[0]["id"], "summary_failed");
    app.on_key(crate::keys::Key::Char(' '), clock.now());
    assert!(rows(&app).contains(&CHECKED.to_owned()));
    let grown = rows(&app);
    goto(&mut app, &clock, grown.len() - 2);
    let out = sent(app.on_key(crate::keys::Key::Enter, clock.now()));
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0]["args"],
        json!({"seq": 3, "summarise": false}),
        "without summary turns it off"
    );
    // Cancel closes the error: the view stays open on its point with
    // the button back.
    refuse(&mut app, &out[0]["id"], "summary_failed");
    let grown = rows(&app);
    goto(&mut app, &clock, grown.len() - 1);
    assert_eq!(
        app.on_key(crate::keys::Key::Enter, clock.now()),
        Effect::None
    );
    let grown = rows(&app);
    assert_eq!(
        grown[grown.len() - 2..],
        [CHECKED.to_owned(), ACTION.to_owned()]
    );
    assert!(app.session_view_screen(80).is_some());
}

#[test]
fn stream_closed_while_pending_keeps_the_old_session_with_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = attached();
    let lines = open(&mut app, &clock);
    accept(&mut app, &lines[0]["id"], segment_a());
    let out = send_rewind(&mut app, &clock);
    let _ = out;
    let answered = app.on_line(hub_closed(SESSION));
    assert!(answered.is_empty());
    // The terminal keeps the old session, the notice stands, and the
    // send still waits: Esc stays ignored.
    assert_eq!(
        app.session().map(|session| session.0.as_str()),
        Some(SESSION)
    );
    assert!(app.notices().iter().any(|line| matches!(line,
        crate::app::NoticeLine::Notice { code, .. } if code == "stream_closed")));
    assert!(app.session_view_screen(80).is_some());
    assert_eq!(app.on_key(crate::keys::Key::Esc, clock.now()), Effect::None);
    assert!(app.session_view_screen(80).is_some());
}

/// The whole open view as text at 80x24.
fn shot(app: &crate::app::App) -> String {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let frame = app.session_view_screen(80).expect("the view is open");
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    let mut targets = Vec::new();
    crate::swapped::render(&frame, area, &mut buffer, &mut targets);
    (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
