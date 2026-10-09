//! Tests for the dropped connection: the backoff, the banner and its
//! attempt number, which failure is a notice, and what a new connection
//! sends again (`docs/tui.md`, "A dropped connection").

use std::path::PathBuf;
use std::time::Duration;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{App, Effect, Link, Phase};
use super::UNANSWERED;
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        version: "0.0.1".to_owned(),
        logo_glyph: "⌇".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` at `schema`.
fn hello_at(schema: u32) -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: schema,
        payload: serde_json::Map::new(),
    })
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    hello_at(contract::SCHEMA_VERSION)
}

/// A `hub_hello` for a schema newer than this terminal reads.
fn newer() -> Line {
    hello_at(contract::SCHEMA_VERSION + 1)
}

/// The banner for attempt `n`.
fn banner(n: u64) -> Option<String> {
    Some(format!("Connection lost · reconnecting (attempt {n})…"))
}

/// One failed connect, as the loop handles it: the notice, then the
/// permit's delay.
fn failed_connect(app: &mut App, error: &str) -> Option<Duration> {
    app.connect_failed(format!("Could not reach the hub: {error}"));
    app.next_retry()
}

#[test]
fn retry_after_doubles_from_half_a_second_and_caps_at_thirty() {
    let mut app = App::new(PathBuf::from("/w"));
    let delays: Vec<Option<Duration>> = (1..=8).map(|_| app.next_retry()).collect();
    let want: Vec<Option<Duration>> = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
        .into_iter()
        .map(|ms| Some(Duration::from_millis(ms)))
        .collect();
    assert_eq!(delays, want);
}

#[test]
fn failures_saturate_at_the_maximum() {
    let mut app = App::new(PathBuf::from("/w"));
    app.reconnect.failures = u32::MAX;
    assert_eq!(app.next_retry(), Some(Duration::from_secs(30)));
    assert_eq!(app.reconnect.failures, u32::MAX);
    // Never up: the attempt number saturates too.
    app.link = Link::Down;
    assert_eq!(app.banner(), banner(u64::from(u32::MAX)));
}

#[test]
fn a_hub_hello_resets_the_count() {
    let mut app = App::new(PathBuf::from("/w"));
    for _ in 0..3 {
        failed_connect(&mut app, "refused");
    }
    app.on_line(hello());
    assert_eq!(app.reconnect.failures, 0);
    app.disconnected();
    assert_eq!(app.next_retry(), Some(Duration::from_millis(500)));
}

#[test]
fn a_refused_schema_never_retries() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(newer());
    assert_eq!(app.link, Link::Refused);
    assert_eq!(app.next_retry(), None);
    assert_eq!(app.reconnect.failures, 0);
    // The reader's end of the refused stream changes nothing.
    app.disconnected();
    assert_eq!(app.next_retry(), None);
    assert_eq!(app.banner(), None);
}

#[test]
fn a_refused_schema_after_failed_attempts_still_says_why() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "refused");
    failed_connect(&mut app, "refused again");
    assert_eq!(app.notice(), Some("Could not reach the hub: refused"));
    app.on_line(newer());
    assert_eq!(
        app.notice(),
        Some(
            format!(
                "The hub runs schema version {}; this terminal reads {}.",
                contract::SCHEMA_VERSION + 1,
                contract::SCHEMA_VERSION
            )
            .as_str()
        )
    );
}

#[test]
fn the_banner_after_a_drop_names_attempt_one_then_two() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(hello());
    app.disconnected();
    app.next_retry();
    assert_eq!(app.banner(), banner(1));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(2));
}

#[test]
fn before_the_first_connection_the_banner_starts_at_attempt_two() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(2));
    failed_connect(&mut app, "refused");
    assert_eq!(app.banner(), banner(3));
}

#[test]
fn no_banner_while_waiting_up_or_refused() {
    let mut waiting = App::new(PathBuf::from("/w"));
    waiting.reconnect.failures = 1;
    assert_eq!(waiting.link, Link::Waiting);
    assert_eq!(waiting.banner(), None);
    let mut up = App::new(PathBuf::from("/w"));
    up.on_line(hello());
    up.reconnect.failures = 1;
    assert_eq!(up.banner(), None);
    let mut refused = App::new(PathBuf::from("/w"));
    refused.on_line(newer());
    refused.reconnect.failures = 1;
    assert_eq!(refused.banner(), None);
    // The same count while down shows it.
    let mut down = App::new(PathBuf::from("/w"));
    down.on_line(hello());
    down.disconnected();
    down.reconnect.failures = 1;
    assert_eq!(down.banner(), banner(1));
}

#[test]
fn no_banner_at_zero_failures_while_down() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(hello());
    // A failed write: the link is down before the reader's end arrives.
    app.write_failed(&[]);
    assert_eq!(app.link, Link::Down);
    assert_eq!(app.banner(), None);
    app.next_retry();
    assert_eq!(app.banner(), banner(1));
}

#[test]
fn only_the_first_failure_is_a_notice() {
    let mut app = App::new(PathBuf::from("/w"));
    failed_connect(&mut app, "first");
    assert_eq!(app.notice(), Some("Could not reach the hub: first"));
    failed_connect(&mut app, "second");
    assert_eq!(app.notice(), Some("Could not reach the hub: first"));
}

#[test]
fn enter_while_refused_keeps_the_draft_and_holds_nothing() {
    let mut app = App::new(PathBuf::from("/w"));
    app.on_line(newer());
    let now = fakes::clock::FakeClock::new().now();
    for ch in "hi".chars() {
        app.on_key(Key::Char(ch), now);
    }
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "hi");
    assert!(app.held.is_empty());
    assert!(app.pending.is_empty());
}

/// The banner row's y and the conversation's top row, drawn at the app's
/// size.
fn drawn(app: &App) -> (usize, usize) {
    let area = Rect::new(0, 0, app.screen.width(), app.screen.height());
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    let row = text
        .lines()
        .position(|line| line.contains("reconnecting (attempt 1)"))
        .unwrap_or_else(|| panic!("the banner is drawn:\n{text}"));
    let layout = app
        .chrome()
        .layout()
        .unwrap_or_else(|| panic!("the session screen's layout"));
    (row, usize::from(crate::view::chrome::body(&layout).y))
}

#[test]
fn the_banner_row_is_counted_in_conversation_height() {
    for (width, height) in [(160, 40), (100, 30)] {
        let mut app = home();
        app.on_line(hello());
        app.attach(contract::SessionId(S_A.to_owned()));
        app.set_size(width, height);
        let before = app.conversation_height();
        app.disconnected();
        app.next_retry();
        assert_eq!(app.conversation_height(), before - 1, "{width}x{height}");
        // The conversation ends on the row above the banner.
        let (banner, top) = drawn(&app);
        assert_eq!(app.conversation_height(), banner - top, "{width}x{height}");
    }
}

/// A hub answer of `kind` with `payload`.
fn hub(kind: &str, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A line from `session` of `kind` with `payload` at `seq`.
fn from(session: &str, kind: &str, seq: Option<u64>, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: seq.map(|_| contract::ActionId("a_m".to_owned())),
        seq: seq.map(contract::Seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// `session`'s `command_accepted` for `id`.
fn accepted(session: &str, id: &str) -> Line {
    from(session, "command_accepted", None, json!({"command_id": id}))
}

/// `session`'s `command_rejected` for `id` with `code` and `message`.
fn refused(session: &str, id: &str, code: &str, message: &str) -> Line {
    from(
        session,
        "command_rejected",
        None,
        json!({"command_id": id, "code": code, "message": message}),
    )
}

/// A live `session_status` for `session`, named `name`.
fn live(session: &str, name: &str) -> Line {
    from(
        session,
        "session_status",
        None,
        json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// Parses command lines.
fn parsed(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Each line's `command`.
fn names(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["command"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A line's `id`.
fn id_of(line: &Value) -> String {
    line["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id: {line}"))
        .to_owned()
}

/// Writes `lines` as the loop does once each write succeeds.
fn write(app: &mut App, lines: Vec<String>) -> Vec<String> {
    for line in &lines {
        app.wrote(line);
    }
    lines
}

/// What `act` sends, written.
fn sent(app: &mut App, act: impl FnOnce(&mut App) -> Effect) -> Vec<String> {
    let effect = act(app);
    let Effect::Send(lines) = effect else {
        panic!("expected lines to send, got {effect:?}");
    };
    write(app, lines)
}

/// Folds `line` and writes what it sends.
fn fold(app: &mut App, line: Line) -> Vec<String> {
    let lines = app.on_line(line);
    write(app, lines)
}

/// The first `hub_hello`: home's feed and first `recent` page, written.
fn linked(app: &mut App) -> Vec<Value> {
    let lines = parsed(&fold(app, hello()));
    assert_eq!(names(&lines), ["feed", "recent"]);
    lines
}

/// The connection drops and a new one says `hub_hello`: what goes out,
/// written.
fn reconnect(app: &mut App) -> Vec<String> {
    app.disconnected();
    app.next_retry();
    fold(app, hello())
}

/// Types `text` and presses Enter.
fn enter(app: &mut App, text: &str) -> Effect {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::Enter, now)
}

/// Starts `session` with the prompt "hi" on a linked home: the subscribe,
/// `commands` and prompt lines, written.
fn started(app: &mut App, session: &str) -> Vec<Value> {
    let start = parsed(&sent(app, |app| enter(app, "hi")));
    assert_eq!(names(&start), ["start"]);
    let lines = parsed(&fold(
        app,
        hub(
            "command_accepted",
            json!({"command_id": start[0]["id"], "result": {"session_id": session}}),
        ),
    ));
    assert_eq!(names(&lines), ["subscribe", "commands", "prompt"]);
    lines
}

/// The rows home draws.
fn rows(app: &App) -> Vec<String> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(_, line, _)| line).collect())
        .unwrap_or_default()
}

/// The key of the row home draws first.
fn first_key(app: &App) -> u64 {
    app.home_screen()
        .and_then(|screen| screen.rows.first().map(|(key, _, _)| *key))
        .unwrap_or_else(|| panic!("a row"))
}

#[test]
fn the_first_hub_hello_reopens_nothing() {
    let mut app = home();
    app.attach(contract::SessionId(S_A.to_owned()));
    app.wrote(
        &json!({"id": "c_1", "command": "prompt", "session_id": S_A, "args": {}}).to_string(),
    );
    let lines = parsed(&app.on_line(hello()));
    assert_eq!(names(&lines), ["feed", "recent"]);
}

#[test]
fn reconnecting_sends_feed_and_recent_again() {
    let mut app = home();
    let before = linked(&mut app);
    let after = parsed(&reconnect(&mut app));
    assert_eq!(names(&after), ["feed", "recent"]);
    assert_ne!(after[0]["id"], before[0]["id"]);
    assert_ne!(after[1]["id"], before[1]["id"]);
}

#[test]
fn reconnecting_reopens_the_attached_session_and_keeps_the_draft() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    let now = fakes::clock::FakeClock::new().now();
    for ch in "half typed".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], S_A);
    assert_eq!(lines[0]["args"]["level"], "full");
    assert_eq!(lines[1]["command"], "commands");
    assert_eq!(app.session().map(|session| session.0.as_str()), Some(S_A));
    assert_eq!(app.draft(), "half typed");
}

#[test]
fn the_wire_after_hub_hello_is_subscribe_commands_kept_feed_recent() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    let prompt = app
        .reconnect
        .kept
        .first()
        .map(|kept| kept.line.clone())
        .unwrap_or_else(|| panic!("the prompt is kept"));
    let lines = reconnect(&mut app);
    let values = parsed(&lines);
    assert_eq!(
        names(&values),
        ["subscribe", "commands", "prompt", "feed", "recent"]
    );
    // The attached session's kept line needs no subscribe of its own.
    assert_eq!(lines[2], prompt);
}

/// A turn of `session`: its prompt and its reply, durable.
fn turn(session: &str) -> [Line; 2] {
    [
        from(
            session,
            "turn_started",
            Some(0),
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "hi"}]}]}),
        ),
        from(session, "text_completed", Some(1), json!({"text": "hello"})),
    ]
}

/// The conversation's lines as text.
fn conversation(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

#[test]
fn lines_from_before_the_drop_are_not_folded_twice() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    fold(&mut app, accepted(S_A, &id_of(&opening[0])));
    for line in turn(S_A) {
        fold(&mut app, line);
    }
    let once = conversation(&app);
    assert!(once.iter().any(|line| line.contains("hello")), "{once:?}");
    let lines = parsed(&reconnect(&mut app));
    fold(&mut app, accepted(S_A, &id_of(&lines[0])));
    for line in turn(S_A) {
        fold(&mut app, line);
    }
    assert_eq!(conversation(&app), once);
}

#[test]
fn the_open_gate_holds_after_a_reconnect() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    fold(&mut app, accepted(S_A, &id_of(&opening[0])));
    let lines = parsed(&reconnect(&mut app));
    // Before the subscribe's answer, the session's lines are dropped.
    for line in turn(S_A) {
        fold(&mut app, line);
    }
    assert!(conversation(&app).is_empty());
    fold(&mut app, accepted(S_A, &id_of(&lines[0])));
    for line in turn(S_A) {
        fold(&mut app, line);
    }
    assert!(!conversation(&app).is_empty());
}

/// The kept prompt's id after [`started`].
fn prompt_id(lines: &[Value]) -> String {
    id_of(&lines[2])
}

#[test]
fn an_accepted_command_is_not_resent() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    fold(&mut app, accepted(S_A, &prompt_id(&opening)));
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["subscribe", "commands", "feed", "recent"]);
}

#[test]
fn a_rejected_command_is_not_resent() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    fold(
        &mut app,
        refused(S_A, &prompt_id(&opening), "busy", "Busy."),
    );
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["subscribe", "commands", "feed", "recent"]);
}

#[test]
fn an_answer_for_another_id_keeps_the_line() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    fold(&mut app, accepted(S_A, "c_0000000000000000"));
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(
        names(&lines),
        ["subscribe", "commands", "prompt", "feed", "recent"]
    );
}

#[test]
fn subscribe_and_commands_are_not_resent() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(
        names(&lines)
            .iter()
            .filter(|name| *name == "subscribe")
            .count(),
        1
    );
    assert_eq!(
        names(&lines)
            .iter()
            .filter(|name| *name == "commands")
            .count(),
        1
    );
    // Both are the reopen's own, with new ids.
    assert_ne!(lines[0]["id"], opening[0]["id"]);
    assert_ne!(lines[1]["id"], opening[1]["id"]);
}

#[test]
fn a_hub_command_is_not_resent() {
    let mut app = home();
    linked(&mut app);
    app.wrote(&json!({"id": "c_1", "command": "delete", "args": {"session": S_B}}).to_string());
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["feed", "recent"]);
}

#[test]
fn two_drops_without_an_answer_resend_the_line_once_each_time() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    for _ in 0..2 {
        let lines = parsed(&reconnect(&mut app));
        let prompts: Vec<&Value> = lines
            .iter()
            .filter(|line| line["command"] == "prompt")
            .collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["id"], opening[2]["id"]);
    }
}

/// A `cancel` for `session` with id `id`, as written.
fn cancel(id: &str, session: &str) -> String {
    json!({"id": id, "command": "cancel", "session_id": session}).to_string()
}

#[test]
fn a_kept_command_for_another_session_follows_a_summary_subscribe() {
    let mut app = home();
    linked(&mut app);
    app.wrote(&cancel("c_1", S_B));
    let lines = reconnect(&mut app);
    let values = parsed(&lines);
    assert_eq!(names(&values), ["subscribe", "cancel", "feed", "recent"]);
    assert_eq!(values[0]["session_id"], S_B);
    assert_eq!(values[0]["args"]["level"], "summary");
    assert_eq!(lines[1], cancel("c_1", S_B));
}

#[test]
fn two_kept_commands_for_one_other_session_share_one_subscribe() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    app.wrote(&cancel("c_1", S_B));
    app.wrote(&cancel("c_2", S_B));
    let values = parsed(&reconnect(&mut app));
    assert_eq!(
        names(&values),
        [
            "subscribe",
            "commands",
            "prompt",
            "subscribe",
            "cancel",
            "cancel",
            "feed",
            "recent"
        ]
    );
    assert_eq!(values[0]["session_id"], S_A);
    assert_eq!(values[3]["session_id"], S_B);
    assert_eq!(values[3]["args"]["level"], "summary");
    assert_eq!(values[4]["id"], "c_1");
    assert_eq!(values[5]["id"], "c_2");
}

#[test]
fn a_written_close_then_home_then_reconnect_resends_it_and_its_answer_settles() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    let close = parsed(&sent(&mut app, |app| enter(app, "/close")));
    assert_eq!(names(&close), ["close"]);
    assert!(app.session().is_none());
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(
        names(&lines),
        ["subscribe", "prompt", "close", "feed", "recent"]
    );
    assert_eq!(lines[0]["session_id"], S_A);
    assert_eq!(lines[0]["args"]["level"], "summary");
    assert_eq!(lines[2]["id"], close[0]["id"]);
    fold(&mut app, accepted(S_A, &id_of(&close[0])));
    assert!(
        app.reconnect
            .kept
            .iter()
            .all(|kept| kept.id != id_of(&close[0]))
    );
    assert!(
        !app.pending.contains_key(&id_of(&close[0])),
        "the resent close's answer settles its pending entry"
    );
    let again = parsed(&reconnect(&mut app));
    assert!(!names(&again).contains(&"close".to_owned()));
}

#[test]
fn a_rejected_resent_close_from_another_session_notes_and_settles() {
    let mut app = home();
    linked(&mut app);
    started(&mut app, S_A);
    let close = parsed(&sent(&mut app, |app| enter(app, "/close")));
    assert!(app.session().is_none());
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(lines[2]["id"], close[0]["id"]);
    fold(&mut app, refused(S_A, &id_of(&close[0]), "busy", "Busy."));
    assert!(!app.pending.contains_key(&id_of(&close[0])));
    assert!(
        app.reconnect
            .kept
            .iter()
            .all(|kept| kept.id != id_of(&close[0]))
    );
    assert_eq!(app.notice(), Some("Busy."));
    let again = parsed(&reconnect(&mut app));
    assert!(!names(&again).contains(&"close".to_owned()));
}

#[test]
fn a_stop_ask_is_resent_and_its_refusal_notes_the_row() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    let stop = parsed(&sent(&mut app, |app| {
        app.home_click(Spot::Stop(first_key(app)))
    }));
    assert_eq!(names(&stop), ["subscribe", "close"]);
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["subscribe", "close", "feed", "recent"]);
    assert_eq!(lines[1]["id"], stop[1]["id"]);
    fold(
        &mut app,
        refused(S_B, &id_of(&stop[1]), "session_held", "Held."),
    );
    assert_eq!(rows(&app), ["✓  tidy docs  Held."]);
    assert_eq!(app.notice(), Some("Held."));
}

#[test]
fn a_delete_ask_settles_as_unanswered_on_reconnect() {
    let mut app = home();
    let first = linked(&mut app);
    fold(
        &mut app,
        hub(
            "command_accepted",
            json!({"command_id": first[1]["id"], "result": {"sessions": [{
                "session_id": S_B, "ts": 0, "project": "-w", "workspace": "/w",
                "name": "old work", "how": "exited",
            }]}}),
        ),
    );
    assert_eq!(app.home_click(Spot::Stop(first_key(&app))), Effect::None);
    let now = fakes::clock::FakeClock::new().now();
    let delete = parsed(&sent(&mut app, |app| app.on_key(Key::Enter, now)));
    assert_eq!(names(&delete), ["delete"]);
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["feed", "recent"]);
    assert_eq!(rows(&app), [format!("○  old work  {UNANSWERED}")]);
    assert_eq!(app.notice(), Some(UNANSWERED));
}

#[test]
fn a_pending_start_returns_to_the_draft_on_reconnect() {
    let mut app = home();
    linked(&mut app);
    let start = parsed(&sent(&mut app, |app| enter(app, "hi")));
    assert_eq!(names(&start), ["start"]);
    let lines = parsed(&reconnect(&mut app));
    assert_eq!(names(&lines), ["feed", "recent"]);
    assert_eq!(app.phase, Phase::Starting);
    assert_eq!(app.draft(), "hi");
    assert_eq!(app.notice(), Some(UNANSWERED));
    assert!(app.pending.is_empty());
}

#[test]
fn a_duplicate_command_rejection_closes_the_entry_silently() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    let lines = parsed(&reconnect(&mut app));
    fold(&mut app, accepted(S_A, &id_of(&lines[0])));
    fold(
        &mut app,
        refused(S_A, &prompt_id(&opening), "duplicate_command", "Seen."),
    );
    assert!(app.pending.is_empty());
    assert_eq!(app.notice(), Some("Connection lost."));
    assert_eq!(app.draft(), "");
}

#[test]
fn another_rejection_still_returns_the_draft() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    let lines = parsed(&reconnect(&mut app));
    fold(&mut app, accepted(S_A, &id_of(&lines[0])));
    fold(
        &mut app,
        refused(S_A, &prompt_id(&opening), "busy", "Busy."),
    );
    assert!(app.pending.is_empty());
    assert_eq!(app.notice(), Some("Busy."));
    assert_eq!(app.draft(), "hi");
}

#[test]
fn an_unwritten_line_still_fails_back_to_the_draft() {
    let mut app = home();
    linked(&mut app);
    let start = parsed(&sent(&mut app, |app| enter(app, "hi")));
    // The prompt is never written: the write fails.
    let lines = app.on_line(hub(
        "command_accepted",
        json!({"command_id": start[0]["id"], "result": {"session_id": S_A}}),
    ));
    write(&mut app, lines.get(..2).unwrap_or_default().to_vec());
    app.write_failed(lines.get(2..).unwrap_or_default());
    assert_eq!(app.draft(), "hi");
    let again = parsed(&reconnect(&mut app));
    assert_eq!(names(&again), ["subscribe", "commands", "feed", "recent"]);
}

#[test]
fn a_failed_resend_keeps_the_line_and_leaves_the_draft() {
    let mut app = home();
    linked(&mut app);
    let opening = started(&mut app, S_A);
    let id = prompt_id(&opening);
    let resent = reconnect(&mut app)
        .into_iter()
        .find(|line| {
            serde_json::from_str::<Value>(line)
                .map(|line| line["id"] == id)
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("the prompt is resent"));
    // The resend never goes out: the write fails.
    app.write_failed(&[resent]);
    assert_eq!(app.draft(), "");
    assert!(app.pending.contains_key(&id));
    assert!(app.reconnect.kept.iter().any(|kept| kept.id == id));
    let again = parsed(&reconnect(&mut app));
    let prompts: Vec<&Value> = again
        .iter()
        .filter(|line| line["command"] == "prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["id"], id);
    assert_eq!(app.draft(), "");
}

#[test]
fn a_feed_answer_from_before_the_drop_is_ignored() {
    let mut app = home();
    let before = linked(&mut app);
    reconnect(&mut app);
    fold(
        &mut app,
        hub(
            "command_rejected",
            json!({"command_id": before[0]["id"], "code": "internal", "message": "old feed"}),
        ),
    );
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn a_recent_answer_from_before_the_drop_is_ignored() {
    let mut app = home();
    let before = linked(&mut app);
    reconnect(&mut app);
    fold(
        &mut app,
        hub(
            "command_accepted",
            json!({"command_id": before[1]["id"], "result": {"sessions": [{
                "session_id": S_B, "ts": 0, "project": "-w", "workspace": "/w",
                "name": "old work", "how": "exited",
            }]}}),
        ),
    );
    assert!(rows(&app).is_empty());
}

#[test]
fn subs_start_empty_after_a_reconnect() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    // Opened at `full`, then left on home without lowering.
    let open = parsed(&sent(&mut app, |app| {
        app.home_click(Spot::Entry(first_key(app)))
    }));
    assert_eq!(names(&open), ["subscribe", "commands"]);
    fold(&mut app, accepted(S_B, &id_of(&open[0])));
    app.go_home();
    reconnect(&mut app);
    // The new connection holds nothing for it: the stop subscribes first.
    let stop = parsed(&sent(&mut app, |app| {
        app.home_click(Spot::Stop(first_key(app)))
    }));
    assert_eq!(names(&stop), ["subscribe", "close"]);
    assert_eq!(stop[0]["args"]["level"], "summary");
}

#[test]
fn an_open_left_before_its_answer_gates_nothing_after_a_reconnect() {
    let mut app = home();
    linked(&mut app);
    fold(&mut app, live(S_B, "tidy docs"));
    sent(&mut app, |app| app.home_click(Spot::Entry(first_key(app))));
    app.go_home();
    reconnect(&mut app);
    // A request from that session reaches the approval queue.
    fold(
        &mut app,
        from(
            S_B,
            "permission_requested",
            None,
            json!({"request_id": "r_1", "effects": ["executes"], "reversible": true,
                "step": "review", "rule": {"subject": "npm test", "prefix": "npm test"}}),
        ),
    );
    assert!(app.badge().is_some() || app.panel().is_some());
}
