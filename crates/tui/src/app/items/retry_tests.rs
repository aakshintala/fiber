//! Tests for the delegate subscription reconciler and its retry: one
//! wish per delegate session, resent only on `session_not_found` while the
//! parent lists it running (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::time::Duration;

use contract::SessionId;
use contract::clock::Clock;
use serde_json::{Value, json};

use super::super::{App, Effect};
use super::{RETRY, retry_due};
use crate::home::Launch;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const DELEGATE_A: &str = "s_dddddddddddddddd";
const DELEGATE_B: &str = "s_eeeeeeeeeeeeeeee";

/// An app on home at 80x24, drawing the default cards.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `subscribe` lines among `out`.
fn subscribes(out: &[Value]) -> Vec<Value> {
    out.iter()
        .filter(|line| line["command"] == "subscribe")
        .cloned()
        .collect()
}

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    let key = app
        .home_screen()
        .map(|screen| {
            screen
                .rows
                .into_iter()
                .map(|(key, _, _)| key)
                .collect::<Vec<_>>()
        })
        .and_then(|keys| keys.into_iter().next())
        .unwrap_or_else(|| panic!("a row"));
    let out = match app.on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id));
}

/// A live `session_status` for `session` in `state`.
fn live(session: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A session `command_accepted` for `id` from `session`.
fn session_accepted(session: &str, id: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

/// A session `command_rejected` for `id` from `session` with `code`.
fn session_refused(session: &str, id: &str, code: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            (
                "message".to_owned(),
                Value::String("no such session".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub refusal for an in-flight subscribe.
fn hub_refused(id: &str, code: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({
            "command_id": id,
            "code": code,
            "message": "no such session",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Folds a Fiber delegate as job `job` with session `delegate`.
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Folds the attached session's `job_completed` for `job`.
fn complete(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
    ));
}

/// Opens `job`'s item view, returning the parsed lines going out.
fn open(app: &mut App, job: &str) -> Vec<Value> {
    match app.open_item(&contract::JobId(job.to_owned())) {
        Effect::Send(lines) => commands(lines),
        Effect::None => Vec::new(),
        Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends or nothing"),
    }
}

/// Drives the clock: the frame time `now` reads from `clock`.
fn tick(app: &mut App, clock: &std::sync::Arc<fakes::clock::FakeClock>) {
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
}

#[test]
fn retry_due_fires_at_its_moment() {
    let clock = fakes::clock::FakeClock::new();
    let at = clock.now();
    assert_eq!(RETRY, Duration::from_millis(500));
    let moment = at + RETRY;
    let before = moment
        .checked_sub(Duration::from_millis(1))
        .unwrap_or(moment);
    assert!(!retry_due(before, moment));
    assert!(retry_due(moment, moment));
    assert!(retry_due(moment + Duration::from_millis(1), moment));
}

#[test]
fn hub_session_not_found_refusal_retries_the_open_delegate_subscribe() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    let refused_at = clock.now();
    app.on_line(hub_refused(&id, "session_not_found"));
    assert_eq!(app.items_wake(), Some(refused_at + RETRY));
    clock.advance(RETRY);
    tick(&mut app, &clock);
    let retry = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0]["session_id"], DELEGATE_A);
    assert_eq!(retry[0]["args"]["level"], "full");
}

#[test]
fn items_wake_stops_asking_once_the_retry_is_due() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    let refused_at = clock.now();
    app.on_line(hub_refused(&id, "session_not_found"));
    let retry_at = refused_at + RETRY;
    for (last_due, expected) in [
        (
            retry_at
                .checked_sub(Duration::from_millis(1))
                .expect("one millisecond before retry"),
            Some(retry_at),
        ),
        (retry_at, None),
        (retry_at + Duration::from_millis(1), None),
    ] {
        app.items.last_due = Some(last_due);
        assert_eq!(app.items_wake(), expected, "last due {last_due:?}");
    }
}

#[test]
fn a_view_refused_full_is_resent_once_at_the_retry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    assert!(commands(app.items_due(clock.now())).is_empty());
    clock.advance(RETRY.checked_sub(Duration::from_millis(1)).unwrap_or(RETRY));
    assert!(commands(app.items_due(clock.now())).is_empty());
    clock.advance(Duration::from_millis(1));
    let resent = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0]["session_id"], DELEGATE_A);
    assert_eq!(resent[0]["args"]["level"], "full");
    // Once resent, no second resend goes out on later steps.
    clock.advance(RETRY);
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn a_card_refused_summary_is_resent_likewise() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let card = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    let id = subscribes(&card)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    // The card sends nothing more itself: the retry goes out through the
    // reconciler only.
    let again = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    assert!(subscribes(&again).is_empty());
    clock.advance(RETRY);
    let resent = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0]["args"]["level"], "summary");
}

#[test]
fn no_resend_after_the_parent_lists_the_job_completed() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    // The refusal, then the parent's `job_completed`, then the retry
    // moment: the wish is gone with the run.
    complete(&mut app, "j_1");
    clock.advance(RETRY + Duration::from_secs(1));
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn no_resend_with_the_link_down() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    app.disconnected();
    clock.advance(RETRY + Duration::from_secs(1));
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn a_refusal_with_another_code_is_never_resent_and_leaves_no_wish() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "invalid_arguments"));
    clock.advance(RETRY + Duration::from_secs(1));
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(app.items_wake().is_none());
}

#[test]
fn the_wake_is_the_retry_then_nothing_once_in_flight() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    let refused_at = clock.now();
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    assert_eq!(app.items_wake(), Some(refused_at + RETRY));
    // The resend in flight asks no wake.
    clock.advance(RETRY);
    let resent = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&resent).len(), 1);
    assert!(app.items_wake().is_none());
}

#[test]
fn a_close_while_full_is_in_flight_waits_for_its_acknowledgement() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    app.close_item();
    // At several later instants the lowering waits and asks no wake.
    for _ in 0..3 {
        clock.advance(Duration::from_secs(1));
        assert!(commands(app.items_due(clock.now())).is_empty());
        assert!(app.items_wake().is_none());
    }
    app.on_line(session_accepted(DELEGATE_A, &id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["args"]["level"], "summary");
    // Once the lowering is acknowledged the wish is gone.
    let lowering = due[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_accepted(DELEGATE_A, &lowering));
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(app.items_wake().is_none());
}

#[test]
fn two_refusals_keep_two_wishes() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    let card = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    assert_eq!(subscribes(&card).len(), 2);
    tick(&mut app, &clock);
    for line in subscribes(&card) {
        let id = line["id"].as_str().unwrap_or_else(|| panic!("an id"));
        let session = line["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a session"));
        app.on_line(session_refused(session, id, "session_not_found"));
    }
    clock.advance(RETRY);
    let resent = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(resent.len(), 2);
}

#[test]
fn a_wish_the_connection_already_holds_is_dropped_with_nothing_sent() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let acked: Vec<Value> = subscribes(&out);
    assert_eq!(acked.len(), 1);
    let id = acked[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_accepted(DELEGATE_A, &id));
    app.close_item();
    // The lowering goes out and is acknowledged: the held level is the
    // wanted one.
    let lowered = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&lowered).len(), 1);
    let lowering = subscribes(&lowered)[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_accepted(DELEGATE_A, &lowering));
    assert!(commands(app.items_due(clock.now())).is_empty());
}
