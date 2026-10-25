//! Tests for the terminal's launch open: the session `fiber resume <id>`
//! and `fiber continue` name, or the session list `fiber resume` with no
//! id names.

use super::super::{App, Effect};
use crate::OpenAt;
use crate::home::{Level, Spot};
use crate::link::Line;
use contract::SessionId;
use serde_json::{Value, json};
use std::path::PathBuf;

fn id(session: &str) -> SessionId {
    SessionId(session.to_owned())
}

/// An app on home at 80x24, launched to open `open_at`.
fn home_at(open_at: OpenAt) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at,
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// Whether the launch's open is spent: back to home.
fn spent(app: &App) -> bool {
    app.home
        .as_ref()
        .is_some_and(|home| home.launch.open_at == OpenAt::Home)
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

/// A hub `command_accepted` for `id` with `result`.
fn accepted(id: &str, result: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `command_rejected` for `id` with `message`.
fn refused(id: &str, message: &str) -> Line {
    refused_code(id, "invalid_arguments", message)
}

/// A hub `command_rejected` for `id` with `code` and `message`.
fn refused_code(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A `session_status` for `session`, idle and named.
fn status(session: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": "idle",
            "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// One exited `recent` row for `session`.
fn exited(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "exited",
    })
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The keys of the rows home draws.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
        .unwrap_or_default()
}

/// Renders home at 80x24 and takes its targets, so keys move among drawn
/// stops.
fn drawn(app: &mut App) {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
}

#[test]
fn a_session_launch_sends_nothing_before_the_link_is_up() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.session().is_none());
    assert!(!spent(&app));
}

#[test]
fn a_session_launch_opens_with_the_first_feed_and_recent() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["command"], "feed");
    assert_eq!(lines[1]["command"], "recent");
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(lines[2]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[2]["args"]["level"], "summary");
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
    assert!(spent(&app));
}

#[test]
fn a_second_hello_sends_no_second_open() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    assert_eq!(commands(app.on_line(hello())).len(), 3);
    assert!(app.on_line(hello()).is_empty());
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn a_refused_launch_open_shows_the_refusal_on_home() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    let ack = lines[2]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(refused(ack, "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_list_launch_focuses_the_list_after_the_first_recent_page() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // The unscoped answer fans out the project's scoped ask.
    let fanout = commands(app.on_line(accepted(
        recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
    )));
    assert_eq!(fanout.len(), 1);
    assert_eq!(fanout[0]["args"]["project"], "-w");
    assert!(spent(&app));
    drawn(&mut app);
    let row = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
    assert!(spent(&app));
}

#[test]
fn a_list_launch_with_no_rows_leaves_the_box_focused() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(
        app.on_line(accepted(recent, json!({"sessions": []})))
            .is_empty()
    );
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert!(spent(&app));
}

#[test]
fn a_list_launch_with_an_empty_first_page_but_a_live_row_focuses_the_list() {
    let mut app = home_at(OpenAt::List);
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // The empty page still fans out the live row's project.
    let fanout = commands(app.on_line(accepted(recent, json!({"sessions": []}))));
    assert_eq!(fanout.len(), 1);
    assert_eq!(fanout[0]["args"]["project"], "-w");
    assert!(spent(&app));
    drawn(&mut app);
    let row = keys(&app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(row)))
    );
}

#[test]
fn a_list_launch_on_a_rejected_first_page_leaves_the_box_focused() {
    let mut app = home_at(OpenAt::List);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    assert!(app.on_line(refused(recent, "no recent")).is_empty());
    assert_eq!(app.notice(), Some("no recent"));
    drawn(&mut app);
    assert_eq!(app.focused(), None);
    assert!(spent(&app));
}

#[test]
fn a_home_launch_never_focuses_the_list() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.on_line(hello()));
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    // The unscoped answer fans out the project's scoped ask.
    let fanout = commands(app.on_line(accepted(
        recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
    )));
    assert_eq!(fanout.len(), 1);
    assert_eq!(fanout[0]["args"]["project"], "-w");
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

#[test]
fn a_later_recent_page_never_focuses_the_list() {
    let mut app = home_at(OpenAt::List);
    app.list_answered(false, true);
    assert!(!spent(&app));
    drawn(&mut app);
    assert_eq!(app.focused(), None);
}

/// A session `notice` line, as the hub forwards a refused switch.
fn notice(session: &str, message: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "notice".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            (
                "code".to_owned(),
                Value::String("credential_missing".to_owned()),
            ),
            ("message".to_owned(), Value::String(message.to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// One durable tail line numbered `seq`, for the open's tail read.
fn tail_line(
    seq: contract::Seq,
    kind: &str,
    action: Option<contract::ActionId>,
    payload: Value,
) -> contract::Envelope {
    contract::Envelope {
        kind: kind.to_owned(),
        session_id: id("s_aaaaaaaaaaaaaaaa"),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action,
        seq: Some(seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// One turn of durable lines from the log's start: a single tail
/// window fills the read, so one answer installs the preview.
fn tail_log() -> Vec<contract::Envelope> {
    vec![
        tail_line(
            contract::Seq(0),
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "prompt"}]}]}),
        ),
        tail_line(contract::Seq(1), "step_started", None, json!({})),
        tail_line(
            contract::Seq(2),
            "assistant_message_started",
            Some(contract::ActionId("a_m".to_owned())),
            json!({}),
        ),
        tail_line(
            contract::Seq(3),
            "text_completed",
            Some(contract::ActionId("a_m".to_owned())),
            json!({"text": "the last reply holds quokkas"}),
        ),
        tail_line(
            contract::Seq(4),
            "assistant_message_completed",
            Some(contract::ActionId("a_m".to_owned())),
            json!({"outcome": "completed"}),
        ),
        tail_line(
            contract::Seq(5),
            "turn_completed",
            None,
            json!({"outcome": "completed"}),
        ),
    ]
}

/// A tail `history` answer arriving after the last draw still raises
/// `full`: the raise waits on the installed tail, never on another
/// draw or input. A late answer changes nothing, and the `full`
/// subscribe goes out with no further input.
#[test]
fn a_history_answer_after_the_last_draw_still_raises_full() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    let first = lines[2]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let sent = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(sent.len(), 1, "the ack releases commands, got {sent:?}");
    assert_eq!(sent[0]["command"], "commands");
    assert!(app.tail_due().is_some(), "the tail read starts");
    // The tail's window: the read is requested but unanswered.
    assert_eq!(
        app.tail_next(),
        Some(json!({})),
        "the first window reads the tail"
    );
    drawn(&mut app);
    // The tail's `history` answer arrives after the last draw, with
    // no further input: installing it still raises `full`.
    app.tail_lines(tail_log());
    assert_eq!(app.notice(), None);
    app.install_tail();
    let raise = raise_line(&mut app);
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(raise["args"], json!({"level": "full"}));
    assert!(app.take_raise().is_none(), "one raise only");
}

/// A session `command_accepted` line for `id`.
fn session_accepted(session: &str, command: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id(session),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), Value::String(command.to_owned()))]
            .into_iter()
            .collect(),
    })
}

#[test]
fn a_session_launch_with_a_label_puts_it_on_the_first_subscribe() {
    let mut app = home_at(OpenAt::Session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some("work".to_owned()),
    ));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(
        lines[2]["args"],
        json!({"level": "summary", "credential": "work"}),
        "the complete args ride on the first subscribe"
    );
    assert!(spent(&app), "the launch carries the label once");
    // Spent: a second hello sends no second open, label or not.
    assert!(app.on_line(hello()).is_empty());
}

#[test]
fn a_session_launch_without_a_label_sends_no_credential_key() {
    let mut app = home_at(OpenAt::Session(id("s_aaaaaaaaaaaaaaaa"), None));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines[2]["command"], "subscribe");
    assert_eq!(lines[2]["args"], json!({"level": "summary"}));
    assert!(
        lines[2]["args"].get("credential").is_none(),
        "an absent label leaves the key out, never null"
    );
}

#[test]
fn an_expected_full_open_puts_the_label_on_its_summary_subscribe() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
        false,
    ));
    let subscribes: Vec<&Value> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    // The held-`full` open sends `summary` first; the `full` raise
    // follows the first frame.
    assert_eq!(subscribes.len(), 1);
    assert_eq!(
        subscribes[0]["args"],
        json!({"level": "summary", "credential": "work"})
    );
}

#[test]
fn a_row_open_and_a_reconnect_carry_no_label() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let subscribes: Vec<&Value> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    assert_eq!(subscribes.len(), 1);
    assert_eq!(subscribes[0]["args"], json!({"level": "summary"}));
    // An ordinary subscription carries none either.
    let line: Value = serde_json::from_str(
        &app.subscribe(&id("s_aaaaaaaaaaaaaaaa"), crate::home::Level::Summary),
    )
    .unwrap();
    assert_eq!(line["args"], json!({"level": "summary"}));
}

#[test]
fn a_refused_launch_open_with_a_label_fails_home_as_before() {
    let mut app = home_at(OpenAt::Session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some("work".to_owned()),
    ));
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines[2]["args"]["credential"], "work");
    let ack = lines[2]["id"].as_str().unwrap_or_else(|| panic!("ack id"));
    assert!(
        app.on_line(refused(ack, "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("held by another process"));
}

#[test]
fn a_running_refusal_notice_between_acceptances_keeps_the_screen_attached() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
        false,
    ));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    // The first acceptance lands, releasing the `commands` line, then
    // the switch refusal as a notice.
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    assert!(
        app.on_line(notice("s_aaaaaaaaaaaaaaaa", "no credential named `nope`"))
            .is_empty()
    );
    assert_eq!(app.notice(), Some("no credential named `nope`"));
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
    // Historical lines are still gated behind the raise's acceptance.
    assert!(app.on_line(status("s_aaaaaaaaaaaaaaaa")).is_empty());
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    // The first frame releases the raise; its acceptance ends the gate.
    let raise: Value = serde_json::from_str(&app.take_raise().unwrap_or_else(|| panic!("a raise")))
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["args"]["level"], "full");
    let last = raise["id"].as_str().unwrap_or_else(|| panic!("last id"));
    app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", last));
    assert!(!app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn a_gated_non_notice_line_is_dropped_without_a_notice() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(crate::home::Level::Full),
        Some("work".to_owned()),
        false,
    ));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    // A gated line that is not a notice is dropped without becoming
    // one: only an opening notice reports while the replay stays
    // gated, so `||` here would file a spurious `rejected` notice.
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", "unknown"))
            .is_empty()
    );
    assert_eq!(app.notice(), None);
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn an_open_sends_summary_and_no_full() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"], json!({"level": "summary"}));
    // The first stage is in flight: no `full` goes out before its ack.
    assert!(app.take_raise().is_none());
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn an_open_with_summary_expected_sends_no_subscribe() {
    let mut app = home_at(OpenAt::Home);
    let subscribe = app.subscribe(&id("s_aaaaaaaaaaaaaaaa"), Level::Summary);
    let subscribe: Value = serde_json::from_str(&subscribe).unwrap_or_else(|err| panic!("{err}"));
    let ack = subscribe["id"]
        .as_str()
        .unwrap_or_else(|| panic!("summary id"));
    app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", ack));
    let lines =
        commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), Some(Level::Summary), None, false));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "commands");
    // The raise is due at once: the first frame releases it.
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    let raise: Value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(raise["args"], json!({"level": "full"}));
    assert!(app.take_raise().is_none(), "one raise only");
}

/// A non-attached session held at `full` with nothing in flight and a
/// live row: reconcile's setup. The acceptance itself would reconcile
/// on a live link, so it goes out while the link is down; the setup
/// ends with the link up and queues no lowering line.
fn full_with_live_row(app: &mut App, session: &str) {
    app.on_line(status(session));
    let subscribe: Value = serde_json::from_str(&app.subscribe(&id(session), Level::Full))
        .unwrap_or_else(|err| panic!("{err}"));
    let ack = subscribe["id"]
        .as_str()
        .unwrap_or_else(|| panic!("full id"));
    app.outbox.link = crate::app::Link::Down;
    app.on_line(session_accepted(session, ack));
    app.outbox.link = crate::app::Link::Up;
    assert!(
        app.home.as_mut().unwrap().subs.take_outbox().is_empty(),
        "setup queues no lowering line"
    );
}

#[test]
fn reconcile_lowers_a_full_live_row_when_the_link_is_up() {
    let mut app = home_at(OpenAt::Home);
    app.on_line(hello());
    full_with_live_row(&mut app, "s_aaaaaaaaaaaaaaaa");
    // Past the gate: the id answers no open, so only the lowering
    // line can appear.
    app.subscribe_accepted(&id("s_aaaaaaaaaaaaaaaa"), "no-such-subscribe");
    let queued = app.home.as_mut().unwrap().subs.take_outbox();
    assert_eq!(queued.len(), 1);
    let line: Value = serde_json::from_str(&queued[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "subscribe");
    assert_eq!(line["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(line["args"], json!({"level": "summary"}));
}

#[test]
fn reconcile_queues_nothing_when_the_link_is_down() {
    let mut app = home_at(OpenAt::Home);
    app.on_line(hello());
    full_with_live_row(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.outbox.link = crate::app::Link::Down;
    app.subscribe_accepted(&id("s_aaaaaaaaaaaaaaaa"), "no-such-subscribe");
    assert!(
        app.home.as_mut().unwrap().subs.take_outbox().is_empty(),
        "a lost connection lowers nothing"
    );
}

/// The raise a frame releases, parsed from its line.
fn raise_line(app: &mut App) -> Value {
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"))
}

#[test]
fn the_first_stage_ack_keeps_the_gate_and_makes_the_raise_due() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        None,
        Some("work".to_owned()),
        false,
    ));
    assert_eq!(
        lines[0]["args"],
        json!({"level": "summary", "credential": "work"}),
        "the credential rides on the first subscribe only"
    );
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert!(
        !app.subscribe_pending(&id("s_aaaaaaaaaaaaaaaa")),
        "the answered first stage leaves nothing in flight"
    );
    let raise = raise_line(&mut app);
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(
        raise["args"],
        json!({"level": "full"}),
        "the raise never carries a credential"
    );
    assert!(
        app.subscribe_pending(&id("s_aaaaaaaaaaaaaaaa")),
        "Subs shows the raise in flight"
    );
    assert!(app.take_raise().is_none(), "one raise only");
}

#[test]
fn the_raise_ack_ends_the_gate_and_the_first_stage_ack_does_not() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    let raise = raise_line(&mut app);
    let last = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("last id"))
        .to_owned();
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &last))
            .is_empty()
    );
    assert!(!app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert_eq!(app.session(), Some(&id("s_aaaaaaaaaaaaaaaa")));
}

/// The raise ack names its own session and ack: a wrong ack with the
/// right session, or the right ack with another session, acks
/// nothing, so only the raise's own answer folds the pass.
#[test]
fn the_raise_ack_needs_its_own_session_and_ack() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    let raise = raise_line(&mut app);
    let last = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("last id"))
        .to_owned();
    assert!(app.raise_ack(&id("s_aaaaaaaaaaaaaaaa"), &last));
    assert!(
        !app.raise_ack(&id("s_aaaaaaaaaaaaaaaa"), "another-ack"),
        "the ack must match"
    );
    assert!(
        !app.raise_ack(&id("s_bbbbbbbbbbbbbbbb"), &last),
        "the session must match"
    );
}

/// Home showing one exited row, ready for a row open.
fn home_with_row() -> App {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.on_line(hello()));
    let feed = lines
        .iter()
        .find(|line| line["command"] == "feed")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a feed id"));
    let recent = lines
        .iter()
        .find(|line| line["command"] == "recent")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a recent id"));
    assert!(app.on_line(accepted(feed, json!({}))).is_empty());
    // The unscoped answer fans out the project's scoped ask.
    let fanout = commands(app.on_line(accepted(
        recent,
        json!({"sessions": [exited("s_aaaaaaaaaaaaaaaa", "old work")]}),
    )));
    assert_eq!(fanout.len(), 1);
    assert_eq!(fanout[0]["args"]["project"], "-w");
    app
}

/// The home screen's text, so a failing open's row note is visible.
fn home_text(app: &mut App) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    app.drawn(&targets);
    crate::view::text(&buf)
}

#[test]
fn a_rejected_first_stage_or_raise_fails_the_open() {
    let mut app = home_with_row();
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    assert!(
        app.on_line(refused(first, "held by another process"))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("held by another process"));
    assert!(
        home_text(&mut app).contains("held by another process"),
        "the refusal is the row's note"
    );
    let mut app = home_with_row();
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    let raise = raise_line(&mut app);
    let last = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("last id"))
        .to_owned();
    assert!(app.on_line(refused(&last, "already held")).is_empty());
    assert!(app.session().is_none());
    assert!(app.on_home());
    assert_eq!(app.notice(), Some("already held"));
    assert!(
        home_text(&mut app).contains("already held"),
        "the refusal is the row's note"
    );
}

#[test]
fn home_during_the_open_sends_no_raise() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    assert_eq!(app.leave(), Effect::None, "the level is `summary`");
    assert!(app.on_home());
    assert!(app.take_raise().is_none(), "leaving drops the open");
    assert!(
        !app.subscribe_pending(&id("s_aaaaaaaaaaaaaaaa")),
        "no `full` in flight"
    );
}

#[test]
fn an_open_sends_commands_only_after_the_subscribe_ack() {
    // The `commands` line waits for the subscribe's acceptance: a
    // refused open sends nothing else, so no second process starts
    // behind the refusal.
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    assert!(
        lines.iter().all(|line| line["command"] != "commands"),
        "commands waits for the subscribe ack, got {lines:?}"
    );
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    assert_eq!(send[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    assert!(
        app.on_line(refused(first, "held by another process"))
            .is_empty(),
        "a refused subscribe sends no commands"
    );
}

#[test]
fn an_open_waits_for_an_existing_summary_subscribe() {
    for accepted in [true, false] {
        let mut app = home_at(OpenAt::Home);
        let session = id("s_aaaaaaaaaaaaaaaa");
        let subscribe = app.subscribe(&session, Level::Summary);
        let subscribe: Value =
            serde_json::from_str(&subscribe).unwrap_or_else(|err| panic!("{err}"));
        let ack = subscribe["id"]
            .as_str()
            .unwrap_or_else(|| panic!("summary id"));
        let lines = app.open_session(session.clone(), Some(Level::Summary), None, false);
        assert!(lines.is_empty(), "commands waits for acceptance: {lines:?}");
        assert!(app.take_raise().is_none());
        assert!(app.gated(&session));
        let answer = if accepted {
            session_accepted("s_aaaaaaaaaaaaaaaa", ack)
        } else {
            refused(ack, "held by another process")
        };
        let sent = commands(app.on_line(answer));
        if accepted {
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["command"], "commands");
            assert_eq!(sent[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
        } else {
            assert!(sent.is_empty(), "a refused subscribe sends no commands");
            assert!(app.on_home());
            assert!(app.take_raise().is_none());
        }
    }
}

#[test]
fn an_open_from_a_held_full_sends_summary_then_the_raise_later() {
    let mut app = home_at(OpenAt::Home);
    let lines = commands(app.open_session(
        id("s_aaaaaaaaaaaaaaaa"),
        Some(Level::Full),
        Some("work".to_owned()),
        false,
    ));
    let subscribes: Vec<_> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    // The held-`full` open sends `summary` first; the `full` raise
    // follows the first frame: two subscribes in total across the open.
    assert_eq!(subscribes.len(), 1);
    assert_eq!(
        subscribes[0]["args"],
        json!({"level": "summary", "credential": "work"})
    );
    let first = subscribes[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    let raise = raise_line(&mut app);
    assert_eq!(raise["args"], json!({"level": "full"}));
    let last = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("last id"))
        .to_owned();
    assert!(
        app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", &last))
            .is_empty()
    );
    assert!(!app.gated(&id("s_aaaaaaaaaaaaaaaa")));
}

#[test]
fn a_missing_workspace_refusal_names_the_path_with_no_rail_card() {
    // The hub sends one exact shape for the confirmed case, from its
    // guard and from a starter failure on an already-missing target:
    // no trailing period. The race keeps the wrapper's period and
    // renders verbatim, as the near-miss test pins.
    let message = "Session s_aaaaaaaaaaaaaaaa could not resume: /w no longer exists";
    let mut app = home_with_row();
    let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    assert!(
        app.on_line(refused_code(first, "io_failed", message))
            .is_empty()
    );
    assert!(app.session().is_none());
    assert!(app.on_home());
    let ruled = "Cannot resume: /w no longer exists. Delete removes the session.";
    assert_eq!(app.notice(), Some(ruled), "{message}");
    let sessions = &app.home.as_ref().unwrap_or_else(|| panic!("home")).sessions;
    let row = sessions
        .row(&id("s_aaaaaaaaaaaaaaaa"))
        .unwrap_or_else(|| panic!("the row stays: {message}"));
    assert!(
        matches!(row.left, Some(crate::home::Left::Exited)),
        "the row stays under exited: {message}"
    );
    assert_eq!(
        row.note.as_deref(),
        Some(ruled),
        "the row's note: {message}"
    );
    assert!(sessions.cards().is_empty(), "no rail card: {message}");
}

#[test]
fn a_near_miss_io_failed_refusal_renders_verbatim() {
    for (code, message) in [
        (
            "io_failed",
            "Session s_aaaaaaaaaaaaaaaa could not resume: boom.",
        ),
        ("io_failed", "it did not bind its socket."),
        (
            "io_failed",
            "Session  could not resume: /w no longer exists",
        ),
        (
            "io_failed",
            "Session s_aaaaaaaaaaaaaaaa could not resume:  no longer exists",
        ),
        // The race keeps the starter-error wrapper's trailing period,
        // so it renders verbatim like any other refusal.
        (
            "io_failed",
            "Session s_aaaaaaaaaaaaaaaa could not resume: /w no longer exists.",
        ),
        (
            "session_not_found",
            "Session s_aaaaaaaaaaaaaaaa could not resume: /w no longer exists",
        ),
    ] {
        let mut app = home_with_row();
        let lines = commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), None, None, false));
        let first = lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("first id"));
        assert!(app.on_line(refused_code(first, code, message)).is_empty());
        assert!(app.session().is_none());
        assert!(app.on_home());
        assert_eq!(app.notice(), Some(message), "{code}: {message}");
        let row = app
            .home
            .as_ref()
            .unwrap_or_else(|| panic!("home"))
            .sessions
            .row(&id("s_aaaaaaaaaaaaaaaa"))
            .unwrap_or_else(|| panic!("the row stays: {code}: {message}"));
        assert_eq!(
            row.note.as_deref(),
            Some(message),
            "the row's note renders verbatim: {code}: {message}"
        );
    }
}

#[test]
fn an_openings_commands_answer_fills_the_slash_list_while_its_replay_is_gated() {
    let mut app = home_at(OpenAt::Home);
    let lines =
        commands(app.open_session(id("s_aaaaaaaaaaaaaaaa"), Some(Level::Full), None, false));
    let first = lines[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("first id"));
    let send = commands(app.on_line(session_accepted("s_aaaaaaaaaaaaaaaa", first)));
    assert_eq!(send.len(), 1, "the ack releases commands, got {send:?}");
    assert_eq!(send[0]["command"], "commands");
    let answer = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: id("s_aaaaaaaaaaaaaaaa"),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"command_id": send[0]["id"], "result": {"commands": [
            {"name": "tdd", "description": "Runs tdd.", "tag": "skill"}]}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    });
    assert!(app.on_line(answer).is_empty());
    // The replay stays gated, and the skill is on the list already.
    assert!(app.gated(&id("s_aaaaaaaaaaaaaaaa")));
    assert!(
        app.completion
            .slash_rows
            .iter()
            .any(|row| row.name == "tdd")
    );
}
