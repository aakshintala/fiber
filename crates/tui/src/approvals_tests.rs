//! Tests for the approval queue: folding, the panel, keys and answers.

use super::{PanelKey, Queue};
use crate::keys::Key;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

/// One session envelope.
fn envelope(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: &str,
) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A `tool_call_requested` for `action`.
fn call(
    session: &str,
    action: &str,
    name: &str,
    arguments: serde_json::Value,
) -> contract::Envelope {
    envelope(
        session,
        "tool_call_requested",
        serde_json::json!({"name": name, "arguments": arguments}),
        action,
    )
}

/// A `permission_requested` with `keys` beside its request id.
fn request(session: &str, action: &str, id: &str, keys: serde_json::Value) -> contract::Envelope {
    let mut payload =
        serde_json::json!({"request_id": id, "effects": ["executes"], "reversible": true});
    if let (Some(into), Some(from)) = (payload.as_object_mut(), keys.as_object()) {
        into.extend(from.clone());
    }
    envelope(session, "permission_requested", payload, action)
}

/// A standing ask by a global rule on `prefix`.
fn standing_ask(session: &str, action: &str, id: &str, prefix: &str) -> contract::Envelope {
    request(
        session,
        action,
        id,
        serde_json::json!({"step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": prefix}}),
    )
}

/// A review request with `extra` keys (`escalation`, `rule`).
fn review(session: &str, id: &str, extra: serde_json::Value) -> contract::Envelope {
    let mut keys = serde_json::json!({"step": "review"});
    if let (Some(into), Some(from)) = (keys.as_object_mut(), extra.as_object()) {
        into.extend(from.clone());
    }
    request(session, "a_9", id, keys)
}

/// A review request offering the rule `npm test`.
fn offering(session: &str, id: &str) -> contract::Envelope {
    review(
        session,
        id,
        serde_json::json!({"rule": {"subject": "npm test --watch", "prefix": "npm test"}}),
    )
}

/// A `permission_resolved` for `id`.
fn resolved(session: &str, id: &str) -> contract::Envelope {
    envelope(
        session,
        "permission_resolved",
        serde_json::json!({"request_id": id, "decision": "deny", "decided_by": "cancel"}),
        "a_1",
    )
}

/// A queue holding `lines`, folded in order.
fn folded(lines: &[contract::Envelope]) -> Queue {
    let mut queue = Queue::default();
    for line in lines {
        queue.fold(line);
    }
    queue
}

/// The panel's lines, or none when it is closed.
fn panel(queue: &Queue) -> Vec<String> {
    queue.panel().map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's line at `row`.
fn row(queue: &Queue, row: usize) -> String {
    panel(queue).get(row).cloned().unwrap_or_default()
}

/// The panel's header line.
fn header(queue: &Queue) -> String {
    row(queue, 0)
}

/// Presses `key` `times` times on the open panel.
fn press(queue: &mut Queue, key: Key, times: usize) {
    for _ in 0..times {
        assert_eq!(queue.on_key(&key), Some(PanelKey::Handled), "{key:?}");
    }
}

/// Answers the shown request as `c_1`, returning the parsed line.
fn answer(queue: &mut Queue) -> serde_json::Value {
    assert_eq!(queue.on_key(&Key::Enter), Some(PanelKey::Answer));
    let line = queue.answer("c_1").unwrap_or_default();
    serde_json::from_str(&line).unwrap_or_default()
}

#[test]
fn a_standing_ask_shows_the_rule_the_call_and_no_remembering_rows() {
    let queue = folded(&[
        call(
            S_A,
            "a_1",
            "shell",
            serde_json::json!({"command": "echo hi"}),
        ),
        standing_ask(S_A, "a_1", "r_1", "echo hi"),
    ]);
    assert_eq!(
        panel(&queue),
        vec![
            format!("approval · {S_A} · 1 of 1"),
            "asked by a global rule: echo hi".to_owned(),
            r#"shell {"command":"echo hi"}"#.to_owned(),
            "› allow once".to_owned(),
            "  deny · type to add feedback".to_owned(),
        ]
    );
    assert_eq!(queue.panel().map(|panel| panel.alert), Some(false));
}

#[test]
fn a_request_offering_a_rule_shows_both_remembering_rows() {
    let queue = folded(&[offering(S_A, "r_1")]);
    assert_eq!(
        panel(&queue),
        vec![
            format!("approval · {S_A} · 1 of 1"),
            "no rule allows this call".to_owned(),
            "› allow once".to_owned(),
            "  allow for this session: npm test".to_owned(),
            "  always allow in this project: npm test".to_owned(),
            "  deny · type to add feedback".to_owned(),
        ]
    );
}

#[test]
fn each_choice_sends_its_answer() {
    let cases = [
        (
            0,
            serde_json::json!({"request_id": "r_1", "decision": "allow"}),
        ),
        (
            1,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "session", "prefix": "npm test"}}),
        ),
        (
            2,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "project", "prefix": "npm test"}}),
        ),
        (
            3,
            serde_json::json!({"request_id": "r_1", "decision": "deny"}),
        ),
    ];
    for (downs, args) in cases {
        let mut queue = folded(&[offering(S_A, "r_1")]);
        press(&mut queue, Key::Down, downs);
        assert_eq!(
            answer(&mut queue),
            serde_json::json!({"id": "c_1", "command": "reply", "session_id": S_A, "args": args}),
            "{downs} down"
        );
        assert!(queue.panel().is_none());
        assert!(queue.badge(0).is_none());
    }
}

#[test]
fn feedback_goes_with_deny_only_when_it_has_text() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    press(&mut queue, Key::Char(' '), 3);
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_1", "decision": "deny"})
    );
    queue.fold(&offering(S_A, "r_2"));
    for ch in "use pnpmx".chars() {
        press(&mut queue, Key::Char(ch), 1);
    }
    press(&mut queue, Key::Backspace, 1);
    assert_eq!(row(&queue, 5), "› deny · use pnpm");
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_2", "decision": "deny", "feedback": "use pnpm"})
    );
}

#[test]
fn typing_or_backspace_moves_the_cursor_to_deny() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    press(&mut queue, Key::Char('x'), 1);
    assert!(row(&queue, 5).starts_with('›'));
    press(&mut queue, Key::Up, 3);
    assert_eq!(row(&queue, 2), "› allow once");
    press(&mut queue, Key::Backspace, 1);
    assert_eq!(row(&queue, 5), "› deny · type to add feedback");
}

#[test]
fn up_and_down_stop_at_the_ends() {
    // A standing ask offers no rule: one Down reaches deny, and more stay.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "echo hi")]);
    press(&mut queue, Key::Down, 5);
    assert_eq!(answer(&mut queue)["args"]["decision"], "deny");
    queue.fold(&offering(S_A, "r_2"));
    press(&mut queue, Key::Down, 5);
    press(&mut queue, Key::Up, 2);
    assert_eq!(answer(&mut queue)["args"]["remember"]["scope"], "session");
    queue.fold(&offering(S_A, "r_3"));
    press(&mut queue, Key::Down, 1);
    press(&mut queue, Key::Up, 5);
    assert_eq!(
        answer(&mut queue)["args"],
        serde_json::json!({"request_id": "r_3", "decision": "allow"})
    );
}

#[test]
fn the_panel_says_why_it_asked_and_tints_an_escalation() {
    let cases = [
        (
            serde_json::json!({"escalation": {"cause": "consecutive_blocks", "reason": "rm -rf"}}),
            "the reviewer escalated: rm -rf",
        ),
        (
            serde_json::json!({"escalation": {"cause": "session_blocks", "reason": "too many"}}),
            "the reviewer escalated: too many",
        ),
        (
            serde_json::json!({"escalation": {"cause": "reviewer_failed",
                "error": {"code": "io_failed", "message": "no model"}}}),
            "the reviewer failed: no model",
        ),
    ];
    for (extra, why) in cases {
        let queue = folded(&[review(S_A, "r_1", extra)]);
        assert_eq!(row(&queue, 1), why);
        assert_eq!(queue.panel().map(|panel| panel.alert), Some(true), "{why}");
    }
    let queue = folded(&[review(S_A, "r_1", serde_json::json!({}))]);
    assert_eq!(queue.panel().map(|panel| panel.alert), Some(false));
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"step": "standing_ask",
            "standing_rule": {"scope": "project", "prefix": "git push"}}),
    )]);
    assert_eq!(row(&queue, 1), "asked by a project rule: git push");
}

#[test]
fn an_irreversible_call_says_so_in_the_header() {
    let queue = folded(&[request(
        S_A,
        "a_1",
        "r_1",
        serde_json::json!({"reversible": false, "step": "review"}),
    )]);
    assert_eq!(
        header(&queue),
        format!("approval · {S_A} · 1 of 1 · irreversible")
    );
}

#[test]
fn a_raw_string_argument_shows_as_typed() {
    let queue = folded(&[
        call(S_A, "a_1", "shell", serde_json::json!("not {json")),
        standing_ask(S_A, "a_1", "r_1", "echo hi"),
    ]);
    assert_eq!(row(&queue, 2), "shell not {json");
}

#[test]
fn requests_from_two_sessions_share_one_queue() {
    let mut queue = folded(&[
        call(S_B, "a_1", "read", serde_json::json!({"path": "b"})),
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_B, "a_1", "r_1", "two"),
        standing_ask(S_A, "a_2", "r_2", "three"),
    ]);
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 3"));
    assert_eq!(answer(&mut queue)["session_id"], S_A);
    assert_eq!(header(&queue), format!("approval · {S_B} · 1 of 2"));
    assert_eq!(row(&queue, 2), r#"read {"path":"b"}"#);
    let answered = answer(&mut queue);
    assert_eq!(answered["session_id"], S_B);
    assert_eq!(answered["args"]["request_id"], "r_1");
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_repeated_request_adds_nothing() {
    let line = standing_ask(S_A, "a_1", "r_1", "one");
    let queue = folded(&[line.clone(), line]);
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

/// A queue with three requests, the panel on the first.
fn three() -> Queue {
    folded(&[
        standing_ask(S_A, "a_1", "r_1", "p1"),
        standing_ask(S_A, "a_2", "r_2", "p2"),
        standing_ask(S_A, "a_3", "r_3", "p3"),
    ])
}

#[test]
fn esc_steps_through_the_queue_then_closes() {
    let mut queue = three();
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 3"));
    press(&mut queue, Key::Esc, 1);
    assert_eq!(header(&queue), format!("approval · {S_A} · 2 of 3"));
    press(&mut queue, Key::Esc, 1);
    assert_eq!(header(&queue), format!("approval · {S_A} · 3 of 3"));
    assert!(queue.badge(0).is_none());
    press(&mut queue, Key::Esc, 1);
    assert!(queue.panel().is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("! 3 waiting · /approvals or ⌥A")
    );
    // A closed panel takes no key.
    assert_eq!(queue.on_key(&Key::Esc), None);
    assert_eq!(queue.answer("c_1"), None);
}

#[test]
fn scrolling_keys_pass_through_the_open_panel() {
    let mut queue = three();
    for key in [Key::PageUp, Key::PageDown, Key::End, Key::CtrlC] {
        assert_eq!(queue.on_key(&key), None, "{key:?}");
    }
}

#[test]
fn a_request_after_one_put_aside_waits_behind_the_badge() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::Esc, 1);
    queue.fold(&standing_ask(S_A, "a_2", "r_2", "two"));
    assert!(queue.panel().is_none());
    assert_eq!(
        queue.badge(0).as_deref(),
        Some("! 2 waiting · /approvals or ⌥A")
    );
    // Once the one put aside resolves, a new request opens at itself.
    queue.fold(&resolved(S_A, "r_1"));
    queue.fold(&resolved(S_A, "r_2"));
    assert!(queue.badge(0).is_none());
    queue.fold(&standing_ask(S_A, "a_3", "r_3", "three"));
    assert_eq!(row(&queue, 1), "asked by a global rule: three");
}

#[test]
fn a_request_while_the_panel_is_open_joins_the_queue() {
    let queue = folded(&[
        standing_ask(S_A, "a_1", "r_1", "one"),
        standing_ask(S_A, "a_2", "r_2", "two"),
    ]);
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 2"));
    assert_eq!(row(&queue, 1), "asked by a global rule: one");
}

#[test]
fn open_first_reopens_at_the_first_request_with_its_feedback() {
    assert!(!Queue::default().open_first());
    let mut queue = three();
    press(&mut queue, Key::Char('x'), 1);
    press(&mut queue, Key::Esc, 3);
    assert!(queue.open_first());
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 3"));
    // Feedback typed for a request stays with it while it waits.
    assert_eq!(row(&queue, 3), "› deny · x");
}

#[test]
fn alt_a_moves_to_the_next_wrapping() {
    let mut queue = three();
    press(&mut queue, Key::AltA, 1);
    assert_eq!(header(&queue), format!("approval · {S_A} · 2 of 3"));
    press(&mut queue, Key::AltA, 2);
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 3"));
    // Alone in the queue, it stays.
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::AltA, 1);
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_restored_reply_puts_its_request_back_where_it_was() {
    let mut queue = three();
    press(&mut queue, Key::AltA, 1);
    answer(&mut queue);
    assert_eq!(header(&queue), format!("approval · {S_A} · 2 of 2"));
    queue.restore("c_9");
    assert_eq!(header(&queue), format!("approval · {S_A} · 2 of 2"));
    queue.restore("c_1");
    assert_eq!(header(&queue), format!("approval · {S_A} · 3 of 3"));
    press(&mut queue, Key::AltA, 1);
    assert_eq!(row(&queue, 1), "asked by a global rule: p1");
    press(&mut queue, Key::AltA, 1);
    assert_eq!(row(&queue, 1), "asked by a global rule: p2");
}

#[test]
fn a_restored_reply_reopens_a_closed_panel() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    answer(&mut queue);
    assert!(queue.panel().is_none());
    queue.restore("c_1");
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_reopened_request_is_no_longer_put_aside() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    press(&mut queue, Key::Esc, 1);
    assert!(queue.open_first());
    answer(&mut queue);
    assert!(queue.panel().is_none());
    queue.restore("c_1");
    // Nothing waits put aside, so the request back opens the panel.
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_resolved_request_leaves_the_queue() {
    let mut queue = three();
    // Requests never seen change nothing.
    queue.fold(&resolved(S_A, "r_9"));
    queue.fold(&resolved(S_B, "r_1"));
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 3"));
    answer(&mut queue);
    // An answered request resolving leaves the shown one in place, and a
    // later restore of its reply puts nothing back.
    queue.fold(&resolved(S_A, "r_1"));
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 2"));
    queue.restore("c_1");
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 2"));
    // The shown request resolving elsewhere moves the panel on.
    queue.fold(&resolved(S_A, "r_2"));
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
    queue.fold(&resolved(S_A, "r_3"));
    assert!(queue.panel().is_none());
    assert!(queue.badge(0).is_none());
}

/// The queue through the app: keys, `/approvals`, and replies on the link.
mod through_the_app {
    use super::{S_A, S_B, call, envelope, standing_ask};
    use crate::app::{App, Effect};
    use crate::keys::Key;
    use crate::link::Line;
    use contract::clock::Clock;
    use std::path::PathBuf;

    /// An app connected to the hub and attached to `S_A`.
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

    /// Folds one session envelope.
    fn feed(app: &mut App, line: contract::Envelope) {
        assert!(app.on_line(Line::Session(line)).is_empty());
    }

    /// A `turn_started` with one message.
    fn turn_started(session: &str, text: &str) -> contract::Envelope {
        envelope(
            session,
            "turn_started",
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": text}]}]}),
            "a_0",
        )
    }

    /// The single line an effect sends.
    fn one_line(effect: Effect) -> String {
        match effect {
            Effect::Send(lines) => {
                assert_eq!(lines.len(), 1);
                lines.into_iter().next().unwrap_or_default()
            }
            Effect::None
            | Effect::Quit
            | Effect::ListFiles
            | Effect::FindPause { .. }
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::Exit(_)
            | Effect::Copy(_)
            | Effect::OpenLink(_) => {
                panic!("expected one line")
            }
        }
    }

    /// Parses one command line.
    fn parse(line: &str) -> serde_json::Value {
        serde_json::from_str(line).unwrap_or_default()
    }

    /// Types `text` and presses Enter, returning the effect.
    fn send(app: &mut App, text: &str, now: std::time::Instant) -> Effect {
        for ch in text.chars() {
            assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
        }
        app.on_key(Key::Enter, now)
    }

    /// The panel's lines, or none when it is closed.
    fn panel(app: &App) -> Vec<String> {
        app.panel().map(|panel| panel.lines).unwrap_or_default()
    }

    /// The panel's header line.
    fn header(app: &App) -> String {
        panel(app).first().cloned().unwrap_or_default()
    }

    /// Presses `key` `times` times, each doing nothing visible to the hub.
    fn press(app: &mut App, key: Key, times: usize, now: std::time::Instant) {
        for _ in 0..times {
            assert_eq!(app.on_key(key.clone(), now), Effect::None);
        }
    }

    /// The reply an Enter sends, with its `id` taken out.
    fn reply(app: &mut App, now: std::time::Instant) -> serde_json::Value {
        let mut value = parse(&one_line(app.on_key(Key::Enter, now)));
        let id = value
            .as_object_mut()
            .and_then(|line| line.remove("id"))
            .unwrap_or_default();
        assert!(id.as_str().is_some_and(|id| id.starts_with("c_")));
        value
    }

    #[test]
    fn a_standing_ask_opens_the_panel_and_enter_allows_once() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(
            &mut app,
            call(
                S_A,
                "a_1",
                "shell",
                serde_json::json!({"command": "echo hi"}),
            ),
        );
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "echo hi"));
        assert_eq!(
            panel(&app),
            vec![
                format!("approval · {S_A} · 1 of 1"),
                "asked by a global rule: echo hi".to_owned(),
                r#"shell {"command":"echo hi"}"#.to_owned(),
                "› allow once".to_owned(),
                "  deny · type to add feedback".to_owned(),
            ]
        );
        assert_eq!(app.panel().map(|panel| panel.alert), Some(false));
        assert_eq!(
            reply(&mut app, now),
            serde_json::json!({"command": "reply", "session_id": S_A,
                "args": {"request_id": "r_1", "decision": "allow"}})
        );
        assert!(app.panel().is_none());
        assert!(app.badge().is_none());
    }

    /// An app with three requests, the panel on the first.
    fn three() -> App {
        let mut app = attached();
        for n in 1..=3 {
            feed(
                &mut app,
                standing_ask(S_A, &format!("a_{n}"), &format!("r_{n}"), &format!("p{n}")),
            );
        }
        app
    }

    #[test]
    fn esc_steps_through_the_queue_then_closes() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = three();
        feed(&mut app, turn_started(S_A, "hi"));
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
        // Esc never cancels while the panel is open.
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert_eq!(header(&app), format!("approval · {S_A} · 2 of 3"));
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert_eq!(header(&app), format!("approval · {S_A} · 3 of 3"));
        assert!(app.badge().is_none());
        assert_eq!(app.on_key(Key::Esc, now), Effect::None);
        assert!(app.panel().is_none());
        assert_eq!(
            app.badge().as_deref(),
            Some("! 3 waiting · /approvals or ⌥A")
        );
        // With the panel closed, Esc cancels a busy turn again.
        let line = one_line(app.on_key(Key::Esc, now));
        assert_eq!(parse(&line)["command"], "cancel");
    }

    #[test]
    fn slash_approvals_reopens_at_the_first_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = three();
        for ch in "x".chars() {
            app.on_key(Key::Char(ch), now);
        }
        press(&mut app, Key::Esc, 3, now);
        assert!(app.panel().is_none());
        assert_eq!(send(&mut app, "/approvals ", now), Effect::None);
        assert_eq!(app.draft(), "");
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
        // Feedback typed for a request stays with it while it waits.
        assert_eq!(panel(&app).last().map(String::as_str), Some("› deny · x"));
    }

    #[test]
    fn slash_approvals_with_nothing_waiting_says_so() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        assert_eq!(send(&mut app, "/approvals", now), Effect::None);
        assert_eq!(app.draft(), "");
        assert_eq!(app.notice(), Some("No requests waiting."));
        assert!(app.panel().is_none());
    }

    #[test]
    fn alt_a_opens_and_moves_to_the_next_wrapping() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(app.notice(), Some("No requests waiting."));
        let mut app = three();
        press(&mut app, Key::Esc, 3, now);
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
        press(&mut app, Key::AltA, 1, now);
        assert_eq!(header(&app), format!("approval · {S_A} · 2 of 3"));
        press(&mut app, Key::AltA, 2, now);
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    }

    #[test]
    fn up_down_and_alt_a_do_nothing_to_the_draft_when_closed() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        app.on_key(Key::Char('a'), now);
        press(&mut app, Key::Up, 1, now);
        press(&mut app, Key::Down, 1, now);
        assert_eq!(app.draft(), "a");
        assert!(app.panel().is_none());
    }

    /// The id of the reply an Enter sends.
    fn reply_id(app: &mut App, now: std::time::Instant) -> String {
        parse(&one_line(app.on_key(Key::Enter, now)))["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn a_rejected_reply_reopens_a_closed_panel() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        let id = reply_id(&mut app, now);
        assert!(app.panel().is_none());
        feed(
            &mut app,
            envelope(
                S_A,
                "command_rejected",
                serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
                "a_0",
            ),
        );
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
    }

    #[test]
    fn a_reply_with_the_link_down_sends_nothing_and_keeps_the_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        app.disconnected();
        assert_eq!(app.on_key(Key::Enter, now), Effect::None);
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
    }

    #[test]
    fn a_failed_reply_write_keeps_the_request() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        let line = one_line(app.on_key(Key::Enter, now));
        assert!(app.panel().is_none());
        app.write_failed(&[line]);
        assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
        assert_eq!(app.draft(), "");
    }

    #[test]
    fn the_conversation_gives_up_rows_for_the_panel_and_the_badge() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        app.set_size(60, 20);
        assert_eq!(app.conversation_height(), 19);
        feed(&mut app, standing_ask(S_A, "a_1", "r_1", "one"));
        // Header, why, allow once and deny replace the input line.
        assert_eq!(app.conversation_height(), 16);
        feed(
            &mut app,
            call(S_A, "a_2", "shell", serde_json::json!("w ".repeat(35))),
        );
        feed(&mut app, standing_ask(S_A, "a_2", "r_2", "two"));
        app.on_key(Key::AltA, now);
        // The call wraps onto two rows.
        assert_eq!(app.conversation_height(), 14);
        press(&mut app, Key::Esc, 1, now);
        assert!(app.panel().is_none());
        assert_eq!(app.conversation_height(), 18);
    }

    #[test]
    fn another_sessions_request_joins_and_its_reply_names_that_session() {
        let now = fakes::clock::FakeClock::new().now();
        let mut app = attached();
        feed(&mut app, standing_ask(S_B, "a_1", "r_1", "two"));
        // Another session's other lines stay out.
        feed(&mut app, turn_started(S_B, "hi"));
        assert!(app.lines().is_empty());
        assert_eq!(header(&app), format!("approval · {S_B} · 1 of 1"));
        // Typing goes to the feedback; the draft is untouched.
        for ch in "no".chars() {
            assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
        }
        assert_eq!(app.draft(), "");
        assert_eq!(
            reply(&mut app, now),
            serde_json::json!({"command": "reply", "session_id": S_B,
                "args": {"request_id": "r_1", "decision": "deny", "feedback": "no"}})
        );
    }
}

#[test]
fn ctrl_g_and_ctrl_r_do_nothing_in_the_panel() {
    let mut queue = folded(&[offering(S_A, "r_1")]);
    let before = panel(&queue);
    press(&mut queue, Key::CtrlG, 1);
    press(&mut queue, Key::CtrlR, 1);
    assert_eq!(panel(&queue), before);
}

#[test]
fn the_steering_keys_do_nothing_while_the_panel_is_open() {
    let mut queue = three();
    let before = panel(&queue);
    press(&mut queue, Key::AltUp, 1);
    press(&mut queue, Key::AltDown, 1);
    press(&mut queue, Key::AltX, 1);
    assert_eq!(panel(&queue), before);
    // With the panel closed they are not the panel's.
    assert_eq!(Queue::default().on_key(&Key::AltX), None);
}

#[test]
fn an_unknown_kind_naming_a_waiting_request_changes_nothing() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    queue.fold(&envelope(
        S_A,
        "notice",
        serde_json::json!({"request_id": "r_1", "message": "hi"}),
        "a_1",
    ));
    assert_eq!(header(&queue), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn permission_resolved_still_removes_an_approval() {
    let mut queue = folded(&[standing_ask(S_A, "a_1", "r_1", "one")]);
    queue.fold(&resolved(S_A, "r_1"));
    assert!(queue.panel().is_none());
    assert!(queue.badge(0).is_none());
}
