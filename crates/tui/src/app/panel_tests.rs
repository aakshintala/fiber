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
fn progress_finished_figures_need_a_summary_and_an_end_and_saturate() {
    let job = contract::JobId("j_1".to_owned());
    for (ended, elapsed) in [(1999, 0), (2000, 0), (2001, 1)] {
        let mut app = attached();
        assert_eq!(app.panel_state().finished_figures(&job), None);
        app.panel_state.keep_last(job.clone(), 6, 2000);
        assert_eq!(app.panel_state().finished_figures(&job), None);
        app.on_line(ts_line(SESSION, "delegate_finished", ended, json!({"job_id": "j_1", "text": "done",
            "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0}, "cost": null, "subscription_cost": 0.0}})));
        assert_eq!(app.panel_state().finished_figures(&job), Some((6, elapsed)));
        app.on_line(ts_line(
            SESSION,
            "job_started",
            3000,
            json!({"job_id": "j_1", "description": "again", "output_path": "/tmp/out"}),
        ));
        assert_eq!(app.panel_state().finished_figures(&job), None);
        app.panel_state.keep_last(job.clone(), 1, 3000);
        assert_eq!(app.panel_state().finished_figures(&job), None);
    }
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

/// A `session_started` for `session`, with an optional worktree.
fn session_started(session: &str, worktree: Option<serde_json::Value>) -> Line {
    let mut payload = serde_json::json!({
        "workspace": "/w/tree",
        "variables": {"path": "", "names": [], "source": "inherited"},
    });
    if let Some(worktree) = worktree {
        payload["worktree"] = worktree;
    }
    session_line(session, "session_started", payload)
}

/// A `session_status` naming `model` in `workspace`.
fn status_line(workspace: &str, model: &str) -> Line {
    session_line(
        SESSION,
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": workspace, "project": "-w",
            "state": "idle", "started": 0, "calls": 0, "since": 0,
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

/// A `preamble_built` with `budget`: `None` leaves the key out.
fn budget_line(budget: Option<f64>) -> Line {
    let mut payload = serde_json::json!({
        "reason": "start", "model": "one/model", "context_window": 1000,
        "trigger_at": 800, "thinking": "low",
        "tool_choice": "auto", "cache_lifetime": "5m",
        "system_prompt": "", "tools": [],
    });
    if let Some(budget) = budget {
        payload["budget"] = budget.into();
    }
    session_line(SESSION, "preamble_built", payload)
}

/// Worktree ownership folds only from the attached session's
/// `session_started` event and a later event can clear it
/// (`docs/tui.md`, "Clearing").
#[test]
fn session_started_folds_and_replaces_worktree_state() {
    let mut app = attached();
    app.on_line(session_started(
        OTHER,
        Some(serde_json::json!({
            "path": "/w/other", "branch": "other", "repository": "/w/.git",
        })),
    ));
    assert!(!app.panel_state().worktree());

    app.on_line(session_started(
        SESSION,
        Some(serde_json::json!({
            "path": "/w/tree", "branch": "fiber/tree", "repository": "/w/.git",
        })),
    ));
    assert!(app.panel_state().worktree());
    app.on_line(session_started(SESSION, None));
    assert!(!app.panel_state().worktree());
}

/// Folding `session_started` keeps its exact `workspace`: `/pwd` shows
/// the recorded root (`docs/tui.md`, "Slash commands").
#[test]
fn session_started_keeps_its_exact_workspace() {
    let mut app = attached();
    assert_eq!(app.panel_state().workspace(), None);
    app.on_line(session_line(
        SESSION,
        "session_started",
        serde_json::json!({
            "workspace": "/worktree/recorded",
            "variables": {"path": "", "names": [], "source": "inherited"},
        }),
    ));
    assert_eq!(app.panel_state().workspace(), Some("/worktree/recorded"));
}

#[test]
fn budget_comes_from_the_latest_preamble_and_absent_clears_it() {
    let mut app = attached();
    app.on_line(budget_line(Some(2.5)));
    assert_eq!(app.panel_state().budget(), Some(2.5));
    app.on_line(preamble_line("two/model", "high", 2000, 1600));
    assert_eq!(app.panel_state().budget(), None);
}

#[test]
fn model_thinking_window_and_trigger_come_from_the_latest_preamble() {
    let mut app = attached();
    app.on_line(preamble_line("one/model", "low", 1000, 800));
    app.on_line(preamble_line("two/model", "high", 2000, 1600));
    let session = app.session().unwrap();
    let panel = app.panel_state();
    assert_eq!(panel.model(), Some("two/model"));
    assert_eq!(panel.thinking(), Some("high"));
    assert_eq!(panel.window_for(session), Some(2000));
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

/// Turns count completions, not starts: a started turn leaves the count
/// at zero, each completed turn adds one (`docs/tui.md`, "The panel",
/// Session: output speed and turns).
#[test]
fn turns_count_turn_completed() {
    let mut app = attached();
    assert_eq!(app.panel_state().turns(), 0);
    app.on_line(session_line(
        SESSION,
        "turn_started",
        serde_json::json!({"input": []}),
    ));
    assert_eq!(app.panel_state().turns(), 0);
    let completed = session_line(SESSION, "turn_completed", serde_json::json!({}));
    app.on_line(completed.clone());
    app.on_line(completed);
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

/// A `delegate_started` for `job`.
fn delegated_line(job: &str) -> Line {
    session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    )
}

/// A `job_completed` for `job` with `status`.
fn completed_line(job: &str, status: &str) -> Line {
    session_line(
        SESSION,
        "job_completed",
        serde_json::json!({"job_id": job, "status": status}),
    )
}

/// A completed delegate stays on its card as done, while a plain job
/// leaves the fold (`docs/tui.md`, "The panel").
#[test]
fn a_completed_delegate_stays_on_the_card_as_done() {
    let mut app = attached();
    app.on_line(started_line("j_1", "review the diff"));
    app.on_line(delegated_line("j_1"));
    app.on_line(started_line("j_2", "build"));
    app.on_line(completed_line("j_2", "completed"));
    assert_eq!(app.panel_state().jobs().len(), 1);
    app.on_line(completed_line("j_1", "completed"));
    assert!(app.panel_state().running_delegates().is_empty());
    assert!(app.panel_state().running_jobs().is_empty());
    let done = app.panel_state().finished_delegates();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].1, "review the diff");
    assert_eq!(done[0].2, contract::events::Outcome::Completed);
    // The delegate's job entry stays, holding its description.
    assert_eq!(app.panel_state().jobs().len(), 1);
}

/// A resume is a new run of the same job: it runs again, once, last in
/// start order (`docs/delegates.md`, "Identity and resume").
#[test]
fn a_resumed_job_runs_again() {
    let mut app = attached();
    app.on_line(started_line("j_1", "review the diff"));
    app.on_line(delegated_line("j_1"));
    app.on_line(completed_line("j_1", "completed"));
    assert_eq!(app.panel_state().finished_delegates().len(), 1);
    app.on_line(started_line("j_1", "review the diff"));
    let running = app.panel_state().running_delegates();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].1, "review the diff");
    assert!(app.panel_state().finished_delegates().is_empty());
    assert_eq!(app.panel_state().jobs().len(), 1);
}

/// Failed and cancelled delegates keep how they ended.
#[test]
fn failed_and_cancelled_delegates_keep_their_outcome() {
    let mut app = attached();
    app.on_line(started_line("j_1", "first"));
    app.on_line(delegated_line("j_1"));
    app.on_line(started_line("j_2", "second"));
    app.on_line(delegated_line("j_2"));
    app.on_line(completed_line("j_1", "failed"));
    app.on_line(completed_line("j_2", "cancelled"));
    assert!(app.panel_state().running_delegates().is_empty());
    let done = app.panel_state().finished_delegates();
    assert_eq!(done.len(), 2);
    // Start order, not completion order.
    assert_eq!(done[0].1, "first");
    assert_eq!(done[0].2, contract::events::Outcome::Failed);
    assert_eq!(done[1].1, "second");
    assert_eq!(done[1].2, contract::events::Outcome::Cancelled);
}

/// Folds a running delegate `job` of `description`.
fn running_delegate(app: &mut App, job: &str, description: &str) {
    app.on_line(started_line(job, description));
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": format!("s_{job:0>16}"),
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// The Delegates card's offset clamps against the held delegates,
/// running and finished, at both edges (`docs/tui.md`, "The panel").
#[test]
fn delegate_scroll_clamps_over_running_and_finished() {
    let mut app = attached();
    running_delegate(&mut app, "j_1", "one");
    running_delegate(&mut app, "j_2", "two");
    running_delegate(&mut app, "j_3", "three");
    running_delegate(&mut app, "j_4", "four");
    running_delegate(&mut app, "j_5", "five");
    app.on_line(completed_line("j_4", "completed"));
    app.on_line(completed_line("j_5", "completed"));
    assert_eq!(app.panel_state().running_delegates().len(), 3);
    assert_eq!(app.panel_state().finished_delegates().len(), 2);
    // Five held, three shown: down clamps at two.
    for _ in 0..9 {
        app.panel_state.scroll_delegates(false);
    }
    assert_eq!(app.panel_state.delegate_scroll(), 2);
    // Up saturates at the top.
    for _ in 0..9 {
        app.panel_state.scroll_delegates(true);
    }
    assert_eq!(app.panel_state.delegate_scroll(), 0);
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
            .contains_key(&contract::JobId("j_1".to_owned()))
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        "started": 0, "calls": 0, "since": 0,
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
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
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
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::OpenFile(_) => panic!("opening sends"),
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

/// The acknowledgement of the staged open in `out`: ack the first-stage
/// `summary`, then the `full` raise the first frame releases. Only the
/// raise's acknowledgement ends the gate, so one ack would leave the
/// session's later lines gated.
fn ack(app: &mut App, out: &[Value]) -> Vec<String> {
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    let _ = app.on_line(session_accepted(SESSION, &id));
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    let raise: Value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
    let raise_id = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a raise id"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &raise_id))
}

/// Asserts `lines` hold one branch query, returning its id.
fn branch_query(lines: &[String]) -> String {
    let branches: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).unwrap_or_else(|err| panic!("{err}")))
        .filter(|line| line["command"] == "shell")
        .collect();
    assert_eq!(branches.len(), 1, "{lines:?}");
    let line = &branches[0];
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
    // The turn asks `commands` again: answer with no rows so the
    // acknowledgement sends only its branch query.
    let asked = app.on_line(session_line(SESSION, "turn_started", json!({"input": []})));
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let answer = session_line(
        SESSION,
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": []}}),
    );
    assert!(app.on_line(answer).is_empty());
}

#[test]
fn a_refused_branch_query_drops_its_kept_line() {
    let mut app = home();
    let out = opened(&mut app);
    let lines = ack(&mut app, &out);
    let id = branch_query(&lines);
    let query = lines
        .iter()
        .find(|line| {
            serde_json::from_str::<Value>(line)
                .map(|line| line["command"] == "shell")
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("the query: {lines:?}"));
    // The loop writes the query: a reconnect would send it again.
    app.outbox.wrote(query);
    assert!(app.outbox.is_kept(&id));
    app.on_line(refused(&id));
    assert!(!app.outbox.is_kept(&id));
    assert!(app.outbox.pending.is_empty());
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
                "state": "idle", "started": 0, "calls": 0, "since": 0,
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
    let lines = app.on_line(live(SESSION, json!({"state": "idle", "since": 1})));
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
            .any(|row| row.line.to_string() == "test/model")
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
        (json!({"branch": "feat"}), Some("git  feat")),
        (json!({"branch": Value::Null}), Some("git  detached")),
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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

/// The wheel steps the panel three rows at a time, picked not measured,
/// clamping down to the rows past the panel's text rows and saturating
/// up at the top (`docs/tui.md`, "The panel": the panel scrolls when its
/// cards outgrow the screen; `scroll_panel`: by three rows, picked, not
/// measured). Offsets count the reserved Session rows (`docs/tui.md`,
/// "The panel", Session: every row draws from the first frame).
#[test]
fn the_wheel_scrolls_the_panel_by_three_and_clamps() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    // Sixty widget lines after the sixteen Session rows and the blank
    // row: 80 content rows over a 39-row viewport, so the end sits at
    // 41 (the two new card rows moved it from 39).
    for expected in [3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36] {
        app.on_wheel(&down);
        assert_eq!(app.panel_state().scroll(), expected);
    }
    app.on_wheel(&down);
    assert_eq!(app.panel_state().scroll(), 39);
    app.on_wheel(&down);
    assert_eq!(app.panel_state().scroll(), 41);
    app.on_wheel(&down);
    assert_eq!(app.panel_state().scroll(), 41);
    let up = mouse(MouseKind::WheelUp, 140, 20);
    for expected in [38, 35, 32, 29, 26, 23, 20, 17, 14, 11, 8, 5, 2] {
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

/// The stored offset clamps to the rows past the panel's text rows
/// first, so a grown screen moves on the first wheel up (`scroll_panel`;
/// `docs/tui.md`, "The panel": the panel scrolls when its cards outgrow
/// the screen).
#[test]
fn wheel_up_after_growth_moves_on_the_first_wheel() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    for _ in 0..8 {
        app.on_wheel(&down);
    }
    assert_eq!(app.panel_state().scroll(), 24);
    app.set_size(160, 60);
    // The stored 24 draws clamped to 21: the sixteen Session rows moved
    // the taller viewport's end from 19 (the drawn text sits two rows
    // further in, so it still reads the same lines).
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   line 02"
    );
    // Clamped 21 moves to 18, so the display moves, not stuck at 21
    // (24 - 3 = 21 would stick).
    app.on_wheel(&mouse(MouseKind::WheelUp, 140, 20));
    assert_eq!(app.panel_state().scroll(), 18);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   plan \u{b7} tasks"
    );
    // Down moves to the new end at 21.
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 21);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   line 02"
    );
}

/// The stored offset clamps to the rows past the panel's text rows
/// first, so shrunk cards move on the first wheel up (`scroll_panel`;
/// `docs/tui.md`, "The panel": the panel scrolls when its cards outgrow
/// the screen).
#[test]
fn wheel_up_after_shrinkage_moves_on_the_first_wheel() {
    use crate::keys::MouseKind;
    let mut app = panel_app();
    big_widget(&mut app);
    let down = mouse(MouseKind::WheelDown, 140, 20);
    for _ in 0..13 {
        app.on_wheel(&down);
    }
    assert_eq!(app.panel_state().scroll(), 39);
    let lines: Vec<String> = (0..50).map(|n| format!("line {n:02}")).collect();
    app.on_line(session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines}),
    ));
    // Fifty lines draw 53 rows after the Session card's sixteen and
    // the blank row, so the stored 39 draws clamped to 31.
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   line 12"
    );
    // Clamped 31 - 3 = 28, so the display moves, not stuck at 31
    // (39 - 3 = 36 would stick).
    app.on_wheel(&mouse(MouseKind::WheelUp, 140, 20));
    assert_eq!(app.panel_state().scroll(), 28);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   line 09"
    );
    // Down returns toward the shrunk end: 28 + 3 = 31.
    app.on_wheel(&mouse(MouseKind::WheelDown, 140, 20));
    assert_eq!(app.panel_state().scroll(), 31);
    assert_eq!(
        panel_text(&app).lines().nth(1).unwrap_or_default(),
        "   line 12"
    );
}

#[test]
fn running_jobs_leaves_delegates_out_in_start_order() {
    let mut app = attached();
    app.on_line(started_line("j_1", "build"));
    app.on_line(started_line("j_2", "test"));
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": "j_2",
            "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
    app.on_line(started_line("j_3", "lint"));
    let running = app.panel_state().running_jobs();
    let ids: Vec<String> = running.iter().map(|(id, _)| id.0.clone()).collect();
    assert_eq!(ids, ["j_1", "j_3"]);
}

#[test]
fn clicking_a_job_row_opens_that_job() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached();
    app.on_line(started_line("j_1", "build"));
    app.on_line(started_line("j_2", "test"));
    let serial = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        app.on_click(TargetId::Panel(Spot::Job(serial))),
        Effect::None
    );
    assert!(app.item_open());
    assert_eq!(
        app.item_view().map(|view| view.description),
        Some("test".to_owned())
    );
}

#[test]
fn clicking_the_running_counts_lists_them() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached();
    app.on_line(started_line("j_1", "build"));
    app.on_line(session_line(
        SESSION,
        "delegate_started",
        serde_json::json!({"job_id": "j_1",
            "delegate_session_id": "s_cccccccccccccccc",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
    assert_eq!(
        app.on_click(TargetId::Panel(Spot::DelegateList)),
        Effect::None
    );
    assert!(app.session_view_open());
    app.close_session_view();
    assert_eq!(app.on_click(TargetId::Panel(Spot::JobList)), Effect::None);
    assert!(app.session_view_open());
}

#[test]
fn the_model_line_opens_the_picker() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached();
    app.on_line(preamble_line("test/model", "high", 1000, 800));
    assert_eq!(app.on_click(TargetId::Panel(Spot::Model)), Effect::None);
    assert!(app.model_picker_open());
    assert_eq!(
        app.model_picker.open.as_ref().map(|open| open.mode),
        Some(crate::model_picker::Mode::Choose)
    );
}

#[test]
fn the_quota_spot_opens_the_view_and_fetches() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached();
    app.outbox.link = crate::app::Link::Up;
    let lines = match app.on_click(TargetId::Panel(Spot::Quota)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap_or_default())
            .collect::<Vec<_>>(),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("no fetch line"),
    };
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "quota");
    assert_eq!(lines[0]["args"], json!({"fetch": true}));
    assert!(app.session_view_open());
}

/// Folds one envelope of the attached session.
fn fold(app: &mut App, kind: &str, payload: serde_json::Value) {
    app.on_line(session_line(SESSION, kind, payload));
}

/// A call's opening markers: a new message starts the call with no
/// thinking streamed yet.
fn start_call(app: &mut App) {
    fold(app, "assistant_message_started", json!({}));
}

/// Streamed thought of the running call.
fn stream_thinking(app: &mut App) {
    fold(app, "reasoning_started", json!({}));
    fold(app, "reasoning_delta", json!({"text": "hmm"}));
}

/// Start markers and an empty completion never set the flag: only
/// streamed thinking proves the model is thinking now (`docs/tui.md`,
/// "The panel").
#[test]
fn markers_without_streamed_thought_leave_thinking_idle() {
    let mut app = attached();
    assert!(!app.panel_state().thinking_now());
    start_call(&mut app);
    assert!(!app.panel_state().thinking_now());
    fold(&mut app, "reasoning_started", json!({}));
    assert!(!app.panel_state().thinking_now());
    fold(&mut app, "reasoning_completed", json!({"text": ""}));
    assert!(!app.panel_state().thinking_now());
}

/// A streamed thinking delta sets the flag.
#[test]
fn streamed_thinking_sets_thinking_now() {
    let mut app = attached();
    start_call(&mut app);
    assert!(!app.panel_state().thinking_now());
    fold(&mut app, "reasoning_delta", json!({"text": "hmm"}));
    assert!(app.panel_state().thinking_now());
}

/// The first text delta clears the flag.
#[test]
fn first_text_delta_clears_thinking_now() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    fold(&mut app, "assistant_message_delta", json!({"text": "done"}));
    assert!(!app.panel_state().thinking_now());
}

/// The first tool call the model starts emitting clears the flag: the
/// arguments delta, not the completed or running call.
#[test]
fn first_tool_arguments_delta_clears_thinking_now() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    fold(
        &mut app,
        "tool_call_arguments_delta",
        json!({"index": 0, "text": "{"}),
    );
    assert!(!app.panel_state().thinking_now());
}

/// A failed call clears the flag before its retry streams: no stale
/// `∴` survives the failure.
#[test]
fn a_failed_call_clears_thinking_now_before_its_retry() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    fold(
        &mut app,
        "assistant_message_completed",
        json!({"outcome": "failed"}),
    );
    assert!(!app.panel_state().thinking_now());
    // The retry is a new call: its thinking sets the flag again.
    start_call(&mut app);
    assert!(!app.panel_state().thinking_now());
    fold(&mut app, "reasoning_delta", json!({"text": "retrying"}));
    assert!(app.panel_state().thinking_now());
}

/// A completed call clears the flag too.
#[test]
fn a_completed_call_clears_thinking_now() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    fold(
        &mut app,
        "assistant_message_completed",
        json!({"outcome": "completed"}),
    );
    assert!(!app.panel_state().thinking_now());
}

/// A thinking-only turn settles cleared at the turn's end, even though
/// it never produced text, tools, or a message.
#[test]
fn turn_settle_clears_a_thinking_only_turn() {
    let mut app = attached();
    start_call(&mut app);
    fold(&mut app, "reasoning_delta", json!({"text": "hmm"}));
    assert!(app.panel_state().thinking_now());
    fold(&mut app, "turn_completed", json!({"outcome": "completed"}));
    assert!(!app.panel_state().thinking_now());
}

/// After the first clear, later reasoning of the same call stays
/// ignored; the next call's start reopens the flag.
#[test]
fn later_thought_after_a_clear_stays_shut_for_that_call() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    fold(&mut app, "assistant_message_delta", json!({"text": "done"}));
    assert!(!app.panel_state().thinking_now());
    fold(&mut app, "reasoning_started", json!({}));
    fold(&mut app, "reasoning_delta", json!({"text": "afterthought"}));
    assert!(!app.panel_state().thinking_now());
    start_call(&mut app);
    fold(&mut app, "reasoning_delta", json!({"text": "next call"}));
    assert!(app.panel_state().thinking_now());
}

/// The flag is the lifecycle, not the level: streamed thinking with no
/// preamble reports thinking, while the card draws no glyph without a
/// level to hang it on.
#[test]
fn thinking_now_without_a_level_reports_thinking() {
    let mut app = attached();
    assert_eq!(app.panel_state().thinking(), None);
    start_call(&mut app);
    fold(&mut app, "reasoning_delta", json!({"text": "hmm"}));
    assert!(app.panel_state().thinking_now());
    assert_eq!(app.panel_state().thinking(), None);
}

/// A session switch and a reset both clear the flag: the new session
/// has no running call.
#[test]
fn switch_and_reset_clear_thinking_now() {
    let mut app = attached();
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    app.attach(contract::SessionId(OTHER.to_owned()));
    assert!(!app.panel_state().thinking_now());
    app.attach(contract::SessionId(SESSION.to_owned()));
    start_call(&mut app);
    stream_thinking(&mut app);
    assert!(app.panel_state().thinking_now());
    app.panel_state.reset();
    assert!(!app.panel_state().thinking_now());
}
