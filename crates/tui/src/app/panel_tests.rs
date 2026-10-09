//! Tests for the panel's folded state: widgets and going home.

use super::super::{App, Effect};
use crate::home::{Launch, Spot as HomeSpot};
use crate::link::Line;
use serde_json::{Value, json};
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app attached to `SESSION`.
fn attached() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// One envelope of `session` at `ts`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    ts_line(session, kind, 0, payload)
}

/// One envelope of `session` at `ts`.
fn ts_line(session: &str, kind: &str, ts: u64, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An `extension_ui` widget's lines.
fn widget(extension: &str, widget: &str, lines: &[&str]) -> Line {
    session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    )
}

#[test]
fn the_latest_widget_lines_win_in_place() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &["one", "two"]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].extension, "plan");
    assert_eq!(widgets[0].widget, "tasks");
    assert_eq!(widgets[0].lines, vec!["one".to_owned(), "two".to_owned()]);
    assert_eq!(widgets[1].extension, "other");
}

#[test]
fn empty_lines_remove_a_widget() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_line(widget("other", "list", &["x"]));
    app.on_line(widget("plan", "tasks", &[]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 1);
    assert_eq!(widgets[0].extension, "other");
}

#[test]
fn empty_lines_preserve_a_widget_with_the_same_extension() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["remove"]));
    app.on_line(widget("plan", "other", &["keep"]));
    app.on_line(widget("plan", "tasks", &[]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 1);
    assert_eq!(widgets[0].widget, "other");
    assert_eq!(widgets[0].lines, vec!["keep".to_owned()]);
}

#[test]
fn widget_updates_match_both_extension_and_widget() {
    let mut app = attached();
    app.on_line(widget("other", "tasks", &["other"]));
    app.on_line(widget("plan", "tasks", &["original"]));
    app.on_line(widget("plan", "tasks", &["updated"]));
    let widgets = app.panel_state().widgets();
    assert_eq!(widgets.len(), 2);
    assert_eq!(widgets[0].lines, vec!["other".to_owned()]);
    assert_eq!(widgets[1].extension, "plan");
    assert_eq!(widgets[1].lines, vec!["updated".to_owned()]);
}

#[test]
fn a_status_line_is_not_a_widget() {
    let mut app = attached();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "status": "working"}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn going_home_clears_the_panel() {
    let mut app = attached();
    app.on_line(widget("plan", "tasks", &["one"]));
    assert!(!app.panel_state().widgets().is_empty());
    app.go_home();
    assert!(app.panel_state().widgets().is_empty());
}

#[test]
fn another_sessions_widget_is_not_folded() {
    let mut app = attached();
    app.on_line(session_line(
        OTHER,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": ["one"]}),
    ));
    assert!(app.panel_state().widgets().is_empty());
}

/// A `session_status` naming `model` in `workspace`.
fn status_line(workspace: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": workspace, "project": "-w",
            "state": "idle", "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": model, "delegates": 0, "jobs": 0, "clients": 0,
        }),
    )
}

/// A `preamble_built` naming `model` with `thinking`, `window` and
/// `trigger`.
fn preamble_line(model: &str, thinking: &str, window: u64, trigger: u64) -> Line {
    session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": model, "context_window": window,
            "trigger_at": trigger, "thinking": thinking,
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    )
}

/// A `model_changed` to `model` with `thinking`.
fn changed_line(model: &str, thinking: &str) -> Line {
    session_line(
        SESSION,
        "model_changed",
        serde_json::json!({
            "before": {"model": "old/model", "cache_lifetime": "5m"},
            "after": {"model": model, "thinking": thinking, "cache_lifetime": "5m"},
            "source": "driver",
        }),
    )
}

/// A `usage_recorded` of `output` tokens at `ts`.
fn usage_line(output: u64, ts: u64) -> Line {
    ts_line(
        SESSION,
        "usage_recorded",
        ts,
        serde_json::json!({
            "generation_id": "g_1", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": output},
            "input_bytes": 0, "cost": 0.01,
        }),
    )
}

/// Replies at `ts` so a usage at `ts + span` spans `span`.
fn reply(app: &mut App, ts: u64) {
    app.on_line(ts_line(
        SESSION,
        "assistant_message_started",
        ts,
        serde_json::json!({}),
    ));
}

/// An `mcp_server_failed` line for `server`.
fn failed_line(server: &str) -> Line {
    session_line(
        SESSION,
        "mcp_server_failed",
        serde_json::json!({"server": server, "reason": "died",
            "will_restart": false,
            "error": {"code": "mcp_server_unavailable", "message": "died"}}),
    )
}

#[test]
fn model_thinking_window_and_trigger_come_from_the_latest_preamble() {
    let mut app = attached();
    app.on_line(preamble_line("one/model", "low", 1000, 800));
    app.on_line(preamble_line("two/model", "high", 2000, 1600));
    let panel = app.panel_state();
    assert_eq!(panel.model(), Some("two/model"));
    assert_eq!(panel.thinking(), Some("high"));
    assert_eq!(panel.window(), Some(2000));
    assert_eq!(panel.trigger_at(), Some(1600));
}

#[test]
fn model_changed_updates_model_and_thinking() {
    let mut app = attached();
    app.on_line(preamble_line("one/model", "low", 1000, 800));
    app.on_line(changed_line("two/model", "high"));
    let panel = app.panel_state();
    assert_eq!(panel.model(), Some("two/model"));
    assert_eq!(panel.thinking(), Some("high"));
}

#[test]
fn turns_count_turn_started() {
    let mut app = attached();
    assert_eq!(app.panel_state().turns(), 0);
    let started = session_line(SESSION, "turn_started", serde_json::json!({"input": []}));
    app.on_line(started.clone());
    app.on_line(started);
    assert_eq!(app.panel_state().turns(), 2);
}

#[test]
fn output_speed_divides_output_tokens_by_the_reply_span() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn a_zero_span_leaves_speed_out() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 1000));
    assert_eq!(app.panel_state().speed(), None);
}

#[test]
fn a_one_millisecond_span_counts() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 1001));
    assert_eq!(app.panel_state().speed(), Some(500_000));
}

#[test]
fn a_copied_usage_is_not_the_last_reply() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
    app.on_line(ts_line(
        SESSION,
        "usage_recorded",
        9000,
        serde_json::json!({
            "generation_id": "g_2", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 9000},
            "input_bytes": 0, "cost": 0.01, "origin_session_id": "s_cccccccccccccccc",
        }),
    ));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn an_extensions_usage_is_not_the_last_reply() {
    let mut app = attached();
    reply(&mut app, 1000);
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), Some(250));
    app.on_line(ts_line(
        SESSION,
        "usage_recorded",
        9000,
        serde_json::json!({
            "generation_id": "g_3", "model": "test/model",
            "tokens": {"input": 10, "cache_read": 0,
                "cache_write": {}, "output": 9000},
            "input_bytes": 0, "cost": 0.01, "extension": "plan",
        }),
    ));
    assert_eq!(app.panel_state().speed(), Some(250));
}

#[test]
fn a_usage_before_any_message_start_leaves_speed_out() {
    let mut app = attached();
    app.on_line(usage_line(500, 3000));
    assert_eq!(app.panel_state().speed(), None);
}

#[test]
fn a_failed_server_is_down_until_ready() {
    let mut app = attached();
    app.on_line(failed_line("bravo"));
    app.on_line(failed_line("alpha"));
    assert_eq!(
        app.panel_state()
            .down()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["alpha", "bravo"]
    );
    app.on_line(session_line(
        SESSION,
        "mcp_server_ready",
        serde_json::json!({"server": "alpha"}),
    ));
    assert_eq!(
        app.panel_state()
            .down()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["bravo"]
    );
}

#[test]
fn the_latest_status_wins() {
    let mut app = attached();
    app.on_line(status_line("/one", "one/model"));
    app.on_line(status_line("/two", "two/model"));
    assert_eq!(
        app.panel_state()
            .status()
            .map(|status| status.workspace.as_str()),
        Some("/two")
    );
    assert_eq!(
        app.panel_state()
            .status()
            .map(|status| status.model.as_str()),
        Some("two/model")
    );
}

/// A `tool_call_completed` with `changes`.
fn changed_call(changes: serde_json::Value) -> Line {
    session_line(
        SESSION,
        "tool_call_completed",
        serde_json::json!({"status": "completed", "content": [], "changes": changes}),
    )
}

/// A `job_started` of `description`.
fn started_line(id: &str, description: &str) -> Line {
    session_line(
        SESSION,
        "job_started",
        serde_json::json!({"job_id": id, "description": description,
            "output_path": "/tmp/out"}),
    )
}

#[test]
fn changes_sum_per_path_and_details_are_ignored() {
    let mut app = attached();
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 10, "removed": 2},
        {"path": "src/b.rs", "added": 3, "removed": 3},
    ])));
    app.on_line(changed_call(serde_json::json!([
        {"path": "src/a.rs", "added": 1, "removed": 1},
    ])));
    app.on_line(session_line(
        SESSION,
        "tool_call_completed",
        serde_json::json!({"status": "completed", "content": [],
            "details": {"diff": "whatever"}}),
    ));
    assert_eq!(app.panel_state().changes().get("src/a.rs"), Some(&(11, 3)));
    assert_eq!(app.panel_state().changes().get("src/b.rs"), Some(&(3, 3)));
    assert_eq!(app.panel_state().changes().len(), 2);
}

#[test]
fn jobs_count_started_without_completed() {
    let mut app = attached();
    app.on_line(started_line("j_1", "build"));
    app.on_line(started_line("j_2", "test"));
    assert_eq!(app.panel_state().jobs().len(), 2);
    app.on_line(session_line(
        SESSION,
        "job_completed",
        serde_json::json!({"job_id": "j_1", "status": "completed"}),
    ));
    assert_eq!(
        app.panel_state().jobs(),
        &[(contract::JobId("j_2".to_owned()), "test".to_owned())]
    );
}

#[test]
fn a_delegate_is_not_a_job() {
    let mut app = attached();
    app.on_line(started_line("j_1", "build"));
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": "j_1",
            "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
    assert_eq!(app.panel_state().jobs().len(), 1);
    assert!(
        app.panel_state()
            .delegate_jobs()
            .contains(&contract::JobId("j_1".to_owned()))
    );
}

#[test]
fn clicking_jobs_expands_and_collapses() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached();
    assert!(!app.panel_state().jobs_open());
    app.on_click(TargetId::Panel(Spot::Jobs));
    assert!(app.panel_state().jobs_open());
    app.on_click(TargetId::Panel(Spot::Jobs));
    assert!(!app.panel_state().jobs_open());
}

/// An app on home, drawing the default card list.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
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

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Links the app: the feed and recent ids.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// A live `session_status` for `session`, in `state`.
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
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The home rows' keys.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

/// Opens the row with `key`, with the parsed lines going out.
fn open(app: &mut App, key: u64) -> Vec<Value> {
    match app.home_click(HomeSpot::Entry(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => panic!("opening sends"),
    }
}

/// Opens the first home row.
fn open_first(app: &mut App) -> Vec<Value> {
    let key = keys(app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    open(app, key)
}

/// A session `command_accepted` for `id` from `session`.
fn session_accepted(session: &str, id: &str) -> Line {
    session_accepted_with(session, id, serde_json::json!({}))
}

/// A session `command_accepted` for `id` with `result`.
fn session_accepted_with(session: &str, id: &str, result: Value) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
    if !result.is_null() {
        payload.insert("result".to_owned(), result);
    }
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

/// A hub `command_rejected` for `id`.
fn refused(id: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String("no".to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A session `command_rejected` for `id` from `session`.
fn session_refused(session: &str, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("invalid_arguments".to_owned()),
            ),
            ("message".to_owned(), Value::String("no".to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// A hub `session_left` for `session`.
fn left_line(session: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            ("session_id".to_owned(), Value::String(session.to_owned())),
            ("how".to_owned(), Value::String("exited".to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// Opens `SESSION` through home: links, feeds its row, opens it.
fn opened(app: &mut App) -> Vec<Value> {
    linked(app);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    open_first(app)
}

/// The acknowledgement of the last subscribe in `out`.
fn ack(app: &mut App, out: &[Value]) -> Vec<String> {
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id))
}

/// Asserts `lines` hold one branch query, returning its id.
fn branch_query(lines: &[String]) -> String {
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "shell");
    assert_eq!(line["session_id"], SESSION);
    assert_eq!(line["args"]["command"], "git rev-parse --abbrev-ref HEAD");
    assert_eq!(line["args"]["send"], false);
    line["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned()
}

/// Answers the branch query `id` with `output`.
fn answer(app: &mut App, id: &str, output: &str) -> Vec<String> {
    answer_with(
        app,
        id,
        json!({"output": output, "process": shell_end(Some(0), None)}),
    )
}

/// A shell end: `exit_code` or `signal`.
fn shell_end(exit_code: Option<i32>, signal: Option<&str>) -> Value {
    let mut process = serde_json::Map::new();
    match exit_code {
        Some(code) => {
            process.insert("exit_code".to_owned(), code.into());
        }
        None => {
            process.insert("exit_code".to_owned(), Value::Null);
        }
    }
    match signal {
        Some(signal) => {
            process.insert("signal".to_owned(), signal.into());
        }
        None => {
            process.insert("signal".to_owned(), Value::Null);
        }
    }
    process.insert("timed_out".to_owned(), false.into());
    Value::Object(process)
}

/// Answers the branch query `id` with `result`.
fn answer_with(app: &mut App, id: &str, result: Value) -> Vec<String> {
    app.on_line(session_accepted_with(SESSION, id, result))
}

#[test]
fn the_acknowledgement_sends_the_branch_query_once() {
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    assert!(id.starts_with("c_"));
    assert!(
        app.on_line(session_line(SESSION, "turn_started", json!({"input": []})))
            .is_empty()
    );
}

#[test]
fn no_query_before_the_acknowledgement() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    open_first(&mut app);
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    assert!(
        app.on_line(session_line(SESSION, "turn_started", json!({"input": []})))
            .is_empty()
    );
}

#[test]
fn no_query_without_the_session_card() {
    let mut app = home();
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .launch
        .panel_cards = vec!["jobs".to_owned()];
    let out = opened(&mut app);
    assert!(out.iter().any(|line| line["command"] == "subscribe"));
    assert!(ack(&mut app, &out).is_empty());
}

#[test]
fn no_query_without_home() {
    let mut app = App::new(PathBuf::from("/w"));
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(
        app.on_line(session_line(
            SESSION,
            "session_status",
            json!({
                "name": "work", "workspace": "/w", "project": "-w",
                "state": "idle", "since": 0,
                "spend": {"tokens": {"input": 1, "cache_read": 0,
                    "cache_write": {}, "output": 2},
                    "cost": 0.0, "subscription_cost": 0.0},
                "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            })
        ))
        .is_empty()
    );
}

#[test]
fn no_query_with_the_link_down() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    app.disconnected();
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
}

#[test]
fn a_query_waits_while_the_row_has_left_and_goes_with_the_next_line() {
    let mut app = home();
    let out = opened(&mut app);
    app.on_line(left_line(SESSION));
    assert!(ack(&mut app, &out).is_empty());
    let lines = app.on_line(live(SESSION, json!({"state": "idle"})));
    branch_query(&lines);
}

#[test]
fn the_first_status_after_attach_sends_none() {
    let mut app = home();
    let out = opened(&mut app);
    branch_query(&ack(&mut app, &out));
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
}

#[test]
fn a_turn_end_seen_in_status_sends_another() {
    for (busy, settled) in [
        (json!({"state": "streaming"}), json!({"state": "idle"})),
        (
            json!({"state": "tool", "tool": "read"}),
            json!({"state": "idle"}),
        ),
        (json!({"state": "retrying"}), json!({"state": "idle"})),
        (
            json!({"state": "waiting", "waiting": {"request_id": "r_1",
                "kind": "approval", "summary": "run?"}}),
            json!({"state": "idle"}),
        ),
        (json!({"state": "streaming"}), json!({"state": "jobs"})),
    ] {
        let mut app = home();
        let out = opened(&mut app);
        let first = branch_query(&ack(&mut app, &out));
        assert!(app.on_line(live(SESSION, busy)).is_empty());
        assert!(app.on_line(live(SESSION, settled)).is_empty());
        let second = branch_query(&answer(&mut app, &first, "main\n"));
        assert_ne!(first, second);
    }
}

#[test]
fn settled_after_settled_sends_none() {
    let mut app = home();
    let out = opened(&mut app);
    branch_query(&ack(&mut app, &out));
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "jobs"})))
            .is_empty()
    );
}

#[test]
fn a_settled_status_does_not_end_an_already_settled_turn() {
    let mut app = home();
    let out = opened(&mut app);
    let first = branch_query(&ack(&mut app, &out));
    answer(&mut app, &first, "main\n");
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    let ended = branch_query(&app.on_line(live(SESSION, json!({"state": "idle"}))));
    answer(&mut app, &ended, "main\n");
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
}

#[test]
fn busy_after_busy_sends_none() {
    let mut app = home();
    let out = opened(&mut app);
    branch_query(&ack(&mut app, &out));
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "tool", "tool": "read"})))
            .is_empty()
    );
}

#[test]
fn a_replayed_turn_completed_sends_none() {
    let mut app = home();
    let out = opened(&mut app);
    branch_query(&ack(&mut app, &out));
    for ts in [0, 1, 999_999_999] {
        assert!(
            app.on_line(ts_line(
                SESSION,
                "turn_completed",
                ts,
                json!({"outcome": "completed"}),
            ))
            .is_empty()
        );
    }
}

#[test]
fn a_turn_end_while_one_is_in_flight_sends_one_after_the_answer() {
    let mut app = home();
    let out = opened(&mut app);
    let first = branch_query(&ack(&mut app, &out));
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    assert!(
        app.on_line(live(SESSION, json!({"state": "idle"})))
            .is_empty()
    );
    branch_query(&answer(&mut app, &first, "main\n"));
}

#[test]
fn the_answer_sets_the_branch() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    assert!(app.panel_state().branch().is_none());
    assert!(
        answer_with(
            &mut app,
            "c_other",
            json!({"output": "other\n",
            "process": shell_end(Some(0), None)})
        )
        .is_empty()
    );
    assert!(app.panel_state().branch().is_none());
    assert!(answer(&mut app, &id, "  main  \nsecond\n").is_empty());
    assert_eq!(
        app.panel_state().branch(),
        Some(&Branch::Named("main".to_owned()))
    );
}

#[test]
fn head_reads_detached() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    answer(&mut app, &id, "HEAD\n");
    assert_eq!(app.panel_state().branch(), Some(&Branch::Detached));
}

#[test]
fn an_empty_answer_leaves_the_row_out() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    answer(&mut app, &id, "");
    assert_eq!(app.panel_state().branch(), Some(&Branch::Absent));
}

#[test]
fn a_blank_first_line_leaves_the_branch_row_out() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    answer(&mut app, &id, "\nmain\n");
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    assert!(
        crate::view::panel::rows(&app, 40)
            .iter()
            .any(|row| row.line.to_string() == "model  test/model")
    );
    assert_eq!(app.panel_state().branch(), Some(&Branch::Absent));
}

#[test]
fn a_non_zero_exit_leaves_the_row_out() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    answer_with(
        &mut app,
        &id,
        json!({"output": "main\n", "process": shell_end(Some(128), None)}),
    );
    assert_eq!(app.panel_state().branch(), Some(&Branch::Absent));
}

#[test]
fn a_signal_leaves_the_row_out() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    answer_with(
        &mut app,
        &id,
        json!({"output": "main\n", "process": shell_end(None, Some("SIGKILL"))}),
    );
    assert_eq!(app.panel_state().branch(), Some(&Branch::Absent));
}

#[test]
fn a_session_rejection_leaves_the_row_out_and_adds_no_notice() {
    use crate::app::panel::Branch;
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    assert!(app.on_line(session_refused(SESSION, &id)).is_empty());
    assert_eq!(app.panel_state().branch(), Some(&Branch::Absent));
    assert!(app.notices().is_empty());
    assert!(app.input().is_empty());
}

#[test]
fn a_hub_rejection_clears_the_query_so_the_next_turn_end_sends() {
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    assert!(app.on_line(refused(&id)).is_empty());
    assert!(app.panel_state().branch().is_some());
    assert!(
        app.on_line(live(SESSION, json!({"state": "streaming"})))
            .is_empty()
    );
    let lines = app.on_line(live(SESSION, json!({"state": "idle"})));
    branch_query(&lines);
}

#[test]
fn the_answer_adds_no_conversation_item() {
    let mut app = home();
    let out = opened(&mut app);
    let id = branch_query(&ack(&mut app, &out));
    let before: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(answer(&mut app, &id, "main\n").is_empty());
    let after: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert_eq!(before, after);
}

#[test]
fn before_any_answer_the_branch_comes_from_the_status() {
    use crate::app::panel::Spot;
    for (git, branch) in [
        (json!({"branch": "feat"}), Some("branch  feat")),
        (json!({"branch": Value::Null}), Some("branch  detached")),
        (json!({}), None),
    ] {
        let mut app = home();
        linked(&mut app);
        app.attach(contract::SessionId(SESSION.to_owned()));
        let mut state = json!({"state": "idle"});
        state["git"] = git;
        app.on_line(live(SESSION, state));
        let drawn = crate::view::panel::rows(&app, 40);
        let found = drawn.iter().find(|row| row.spot == Some(Spot::Branch));
        assert_eq!(found.map(|row| row.line.to_string()).as_deref(), branch);
    }
}

#[test]
fn clicking_the_branch_runs_git_status_as_bang_bang_does() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = home();
    let out = opened(&mut app);
    branch_query(&ack(&mut app, &out));
    let Effect::Send(lines) = app.on_click(TargetId::Panel(Spot::Branch)) else {
        panic!("a click sends");
    };
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "shell");
    assert_eq!(line["args"]["command"], "git --no-optional-locks status");
    assert_eq!(line["args"]["send"], false);
    let id = line["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    assert!(
        answer_with(
            &mut app,
            &id,
            json!({"output": "M changed\n",
            "process": shell_end(Some(0), None)})
        )
        .is_empty()
    );
    assert!(app.lines().iter().any(|line| {
        line.to_string()
            .contains("! git --no-optional-locks status")
    }));
}

#[test]
fn clicking_the_branch_with_the_link_down_sends_nothing() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = home();
    linked(&mut app);
    app.disconnected();
    app.attach(contract::SessionId(SESSION.to_owned()));
    assert_eq!(app.on_click(TargetId::Panel(Spot::Branch)), Effect::None);
}

/// An app with home state at 160x40, attached.
fn panel_app() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(160, 40);
    app
}

/// Folds a widget of sixty lines.
fn big_widget(app: &mut App) {
    let lines: Vec<String> = (0..60).map(|n| format!("line {n:02}")).collect();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines}),
    ));
}

/// A mouse report of `kind` at 0-based `col`, `row`.
fn mouse(kind: crate::keys::MouseKind, col: u16, row: u16) -> crate::keys::Mouse {
    crate::keys::Mouse { kind, col, row }
}

#[test]
fn the_wheel_scrolls_the_panel_by_three_and_clamps() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    for expected in [3, 6, 9, 12, 15, 18, 21] {
        app.on_wheel(&down);
        assert_eq!(app.panel_state().scroll(), expected);
    }
    app.on_wheel(&down);
    assert_eq!(app.panel_state().scroll(), 22);
    app.on_wheel(&down);
    assert_eq!(app.panel_state().scroll(), 22);
    let up = mouse(MouseKind::WheelUp, 140, 20);
    for expected in [19, 16, 13, 10, 7, 4, 1] {
        app.on_wheel(&up);
        assert_eq!(app.panel_state().scroll(), expected);
    }
    app.on_wheel(&up);
    assert_eq!(app.panel_state().scroll(), 0);
    app.on_wheel(&up);
    assert_eq!(app.panel_state().scroll(), 0);
}

#[test]
fn the_wheel_over_the_conversation_does_not_scroll_the_panel() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 3);
    app.on_wheel(&mouse(MouseKind::WheelDown, 10, 20));
    assert_eq!(app.panel_state().scroll(), 3);
}

#[test]
fn motion_over_the_panel_does_not_scroll_it() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    app.on_wheel(&mouse(MouseKind::Motion, 140, 20));
    assert_eq!(app.panel_state().scroll(), 0);
}

#[test]
fn a_panel_that_fits_never_scrolls() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    app.on_line(widget("plan", "tasks", &["one"]));
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 0);
}

#[test]
fn scrolling_without_a_panel_rect_does_nothing() {
    use crate::keys::MouseKind;
    let mut app = attached();
    app.on_wheel(&mouse(MouseKind::WheelDown, 10, 5));
    app.scroll_panel(false);
    assert_eq!(app.panel_state().scroll(), 0);
}

/// The panel's drawn text: the panel rect drawn alone.
fn panel_text(app: &App) -> String {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let area = ratatui::layout::Rect::new(0, 0, rect.width, rect.height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let mut targets = Vec::new();
    crate::view::panel::draw(app, area, &mut buf, &mut targets);
    crate::view::text(&buf)
}

#[test]
fn wheel_up_after_growth_moves_on_the_first_wheel() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    for _ in 0..8 {
        app.on_wheel(&down);
    }
    assert_eq!(app.panel_state().scroll(), 22);
    app.set_size(160, 60);
    // The stored 22 draws clamped to 2.
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  line 01"
    );
    // Clamped 2 saturates to 0, so the display moves to the top, not
    // stuck at 2 (22 - 3 = 19 would stick).
    app.on_wheel(&mouse(MouseKind::WheelUp, 140, 20));
    assert_eq!(app.panel_state().scroll(), 0);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  plan \u{b7} tasks"
    );
    // Down from the top clamps to the new end: 0 + 3 past 2 stays at 2.
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 2);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  line 01"
    );
}

#[test]
fn wheel_up_after_shrinkage_moves_on_the_first_wheel() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    for _ in 0..8 {
        app.on_wheel(&down);
    }
    assert_eq!(app.panel_state().scroll(), 22);
    let lines: Vec<String> = (0..50).map(|n| format!("line {n:02}")).collect();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines}),
    ));
    // Fifty lines draw 51 rows against 39, so the stored 22 draws
    // clamped to 12.
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  line 11"
    );
    // Clamped 12 - 3 = 9, so the display moves, not stuck at 12
    // (22 - 3 = 19 would stick).
    app.on_wheel(&mouse(MouseKind::WheelUp, 140, 20));
    assert_eq!(app.panel_state().scroll(), 9);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  line 08"
    );
    // Down returns to the shrunk end: 9 + 3 = 12.
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 12);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "  line 11"
    );
}
