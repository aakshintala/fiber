//! Tests for the asides and what shows after a crash, driven through the
//! app.

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

fn feed(app: &mut App, kind: &str, action: Option<&str>, ts: u64, payload: Value) {
    feed_with_seq(app, kind, action, ts, None, payload);
}

fn feed_durable(app: &mut App, kind: &str, ts: u64, payload: Value) {
    feed_with_seq(app, kind, None, ts, Some(contract::Seq(ts)), payload);
}

fn feed_with_seq(
    app: &mut App,
    kind: &str,
    action: Option<&str>,
    ts: u64,
    seq: Option<contract::Seq>,
    payload: Value,
) {
    app.on_line(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

fn start(app: &mut App, ts: u64) {
    feed(
        app,
        "turn_started",
        None,
        ts,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
}

fn request(app: &mut App, action: &str, path: &str, ts: u64) {
    feed(
        app,
        "tool_call_requested",
        Some(action),
        ts,
        json!({"name": "read", "arguments": {"path": path}}),
    );
}

fn resumed(app: &mut App, resumed: bool, ts: u64) {
    feed(
        app,
        "fiber_started",
        None,
        ts,
        json!({"version": "0.0.1", "resumed": resumed}),
    );
}

/// A `rewound` line at `ts`, naming the continuing session.
fn rewound(app: &mut App, ts: u64) {
    feed_durable(
        app,
        "rewound",
        ts,
        json!({"new_session_id": "s_ccc", "seq": 12}),
    );
}

fn exited(app: &mut App, suspended_on: Option<&str>, ts: u64) {
    let mut payload = json!({"exit_code": 0, "usage": {"tokens": {"input": 0, "cache_read": 0,
        "cache_write": {}, "output": 0}, "cost": 0, "subscription_cost": 0}});
    if let (Some(request), Some(object)) = (suspended_on, payload.as_object_mut()) {
        object.insert("suspended_on".to_owned(), json!(request));
    }
    feed(app, "fiber_exited", None, ts, payload);
}

/// A turn with three calls: `a.rs` started, `b.rs` only requested, `c.rs`
/// completed; its last line at 5s.
fn crashed() -> App {
    let mut app = app();
    start(&mut app, 0);
    feed(&mut app, "step_started", None, 0, json!({}));
    request(&mut app, "a_1", "a.rs", 1_000);
    feed(&mut app, "tool_call_started", Some("a_1"), 1_000, json!({}));
    request(&mut app, "a_2", "b.rs", 2_000);
    request(&mut app, "a_3", "c.rs", 3_000);
    feed(&mut app, "tool_call_started", Some("a_3"), 3_000, json!({}));
    feed(
        &mut app,
        "tool_call_completed",
        Some("a_3"),
        5_000,
        json!({"status": "completed", "content": []}),
    );
    app
}

/// Opens every target whose line reads `text`.
fn open(app: &mut App, text: &str) {
    let lines = texts(app);
    let found: Vec<Target> = app
        .targets()
        .into_iter()
        .filter(|(at, _)| lines.get(*at).map(String::as_str) == Some(text))
        .map(|(_, target)| target)
        .collect();
    assert!(!found.is_empty(), "no target reads {text:?}");
    for target in found {
        app.open(target);
    }
}

#[test]
fn a_resumed_process_closes_the_cut_short_turn() {
    let mut app = crashed();
    // While the turn runs, a started call is running.
    let summary = texts(&app)[7].clone();
    open(&mut app, &summary);
    assert_eq!(
        texts(&app)[8],
        "     1 ○ read     a.rs                                                 running"
    );
    // Opening flips the toggle, so the row is read again to close it.
    let summary = texts(&app)[7].clone();
    open(&mut app, &summary);
    resumed(&mut app, true, 60_000);
    let lines = texts(&app);
    assert_eq!(
        lines[lines.len() - 3..],
        [
            " ▣ cut short: Fiber stopped · 5s · 3 calls",
            "▀".repeat(79).as_str(),
            "↺ resumed",
        ]
    );
    open(&mut app, &lines[7]);
    assert_eq!(
        texts(&app)[8..11],
        [
            "     1 ? read     a.rs                           ? may have run; not run again",
            "       ○ read     b.rs                                                 running",
            "       ✓ read     c.rs                                                 0 lines",
        ]
    );
    // The rule fires once: a second resume finds no open turn.
    resumed(&mut app, true, 70_000);
    let again = texts(&app);
    assert_eq!(
        again
            .iter()
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with('▣') || line.starts_with('↺')
            })
            .count(),
        2
    );
    // A new turn after it is open again, with its start time under it
    // and its blank row ahead of its card.
    start(&mut app, 80_000);
    let lines = texts(&app);
    assert_eq!(
        lines[lines.len() - 6..],
        ["", "▄▄▄▄▄", " go ▐", "▀▀▀▀▀", "you 00:01 ", ""]
    );
}

#[test]
fn a_rewound_session_places_the_line_naming_the_new_session() {
    let mut app = crashed();
    rewound(&mut app, 60_000);
    let lines = texts(&app);
    // The old session's `rewound` folds through `turn.rs` into the
    // crash fold's aside, naming the session that continues it; the
    // line also says what was not restored and which jobs stopped
    // (`docs/tui.md`, "Rewind"). The open card indents it.
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start() == "⟲ s_ccc · nothing restored · jobs stopped: none"),
        "{lines:?}"
    );
}

#[test]
fn a_rewound_session_lists_the_jobs_that_stopped() {
    let mut app = crashed();
    // Two jobs still running when the session is rewound: both stop
    // with it (`docs/tui.md`, "Rewind"; `docs/events.md`, "Rewind").
    feed_durable(
        &mut app,
        "job_started",
        10_000,
        json!({"job_id": "j_2", "description": "second task", "output_path": "/tmp/out"}),
    );
    feed_durable(
        &mut app,
        "job_started",
        11_000,
        json!({"job_id": "j_1", "description": "first task", "output_path": "/tmp/out"}),
    );
    // The stop wait writes each cancelled completion before `rewound`.
    feed_durable(
        &mut app,
        "job_completed",
        12_000,
        json!({"job_id": "j_2", "status": "cancelled"}),
    );
    // Hook notices are ephemeral and may appear between durable stops.
    feed(
        &mut app,
        "notice",
        None,
        12_500,
        json!({"code": "hook_failed", "message": "end hook failed"}),
    );
    feed_durable(
        &mut app,
        "job_completed",
        13_000,
        json!({"job_id": "j_1", "status": "cancelled"}),
    );
    // End hooks may also write durable state lines during the stop run.
    feed_durable(
        &mut app,
        "extension_state_set",
        14_000,
        json!({"extension": "example", "key": "status", "value": "stopped"}),
    );
    rewound(&mut app, 60_000);
    let lines = texts(&app);
    // Sorted by description, so the line is stable no matter which
    // `job_started` arrived first.
    assert!(
        lines.iter().any(|line| line.trim_start()
            == "⟲ s_ccc · nothing restored · jobs stopped: first task, second task"),
        "{lines:?}"
    );
}

#[test]
fn a_turn_suspended_on_a_request_resumes_instead() {
    let mut app = crashed();
    exited(&mut app, Some("r_1"), 6_000);
    resumed(&mut app, true, 60_000);
    let lines = texts(&app);
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with('▣') || line.starts_with('↺')),
        "{lines:?}"
    );
    // The suspension covers that one resume only.
    resumed(&mut app, true, 90_000);
    let lines = texts(&app);
    assert_eq!(
        lines[lines.len() - 3..],
        [
            " ▣ cut short: Fiber stopped · 1m 00s · 3 calls",
            "▀".repeat(79).as_str(),
            "↺ resumed",
        ]
    );
}

#[test]
fn an_exit_with_no_request_does_not_suspend_the_rule() {
    let mut app = crashed();
    exited(&mut app, None, 6_000);
    resumed(&mut app, true, 60_000);
    assert_eq!(texts(&app).last().map(String::as_str), Some("↺ resumed"));
}

#[test]
fn a_fresh_process_and_a_completed_turn_cut_nothing_short() {
    let mut app = crashed();
    resumed(&mut app, false, 60_000);
    assert!(!texts(&app).iter().any(|line| line.starts_with('▣')));
    feed(
        &mut app,
        "turn_completed",
        None,
        61_000,
        json!({"outcome": "completed"}),
    );
    resumed(&mut app, true, 90_000);
    let lines = texts(&app);
    assert_eq!(
        lines.get(lines.len().saturating_sub(2)).map(String::as_str),
        Some(" ▣ completed · 1m 01s · 3 calls")
    );
}

fn job_started(app: &mut App, id: &str, description: &str) {
    feed(
        app,
        "job_started",
        None,
        0,
        json!({"job_id": id, "description": description, "output_path": "/tmp/o"}),
    );
}

fn job_completed(app: &mut App, id: &str, code: Option<&str>) {
    let mut payload = json!({"job_id": id, "status": "completed"});
    if let (Some(code), Some(object)) = (code, payload.as_object_mut()) {
        object.insert("status".to_owned(), json!("failed"));
        object.insert(
            "error".to_owned(),
            json!({"code": code, "message": format!("{id} {code}.")}),
        );
    }
    feed(app, "job_completed", None, 0, payload);
}

#[test]
fn orphaned_jobs_are_one_line_naming_them_all() {
    let mut app = app();
    job_started(&mut app, "j_1", "build the docs");
    job_started(&mut app, "j_2", "watch the tests");
    start(&mut app, 0);
    resumed(&mut app, true, 9_000);
    job_completed(&mut app, "j_1", Some("orphaned"));
    job_completed(&mut app, "j_2", None);
    job_completed(&mut app, "j_4", Some("nonzero_exit"));
    job_completed(&mut app, "j_3", Some("orphaned"));
    let line = "Orphaned jobs: build the docs, j_3";
    assert_eq!(
        texts(&app),
        [
            "",
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "you 00:00 ",
            "",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " ▣ cut short: Fiber stopped",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "↺ resumed",
            "Orphaned jobs: build the docs, j_3",
        ]
    );
    open(&mut app, line);
    assert_eq!(
        texts(&app)[11..],
        ["  build the docs: j_1 orphaned.", "  j_3: j_3 orphaned."]
    );
    open(&mut app, line);
    assert_eq!(texts(&app).len(), 11);
    // The next resume starts a line of its own.
    resumed(&mut app, true, 20_000);
    job_completed(&mut app, "j_5", Some("orphaned"));
    assert_eq!(
        texts(&app).last().map(String::as_str),
        Some("Orphaned jobs: j_5")
    );
}

#[test]
fn orphans_found_while_a_turn_resumes_join_its_card() {
    let mut app = app();
    start(&mut app, 0);
    exited(&mut app, Some("r_1"), 1_000);
    resumed(&mut app, true, 9_000);
    job_completed(&mut app, "j_1", Some("orphaned"));
    job_completed(&mut app, "j_2", Some("orphaned"));
    assert_eq!(
        texts(&app),
        [
            "",
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "you 00:00 ",
            "",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " Orphaned jobs: j_1, j_2",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
        ]
    );
    feed(
        &mut app,
        "turn_completed",
        None,
        10_000,
        json!({"outcome": "completed"}),
    );
    assert_eq!(texts(&app)[7], " Orphaned jobs: j_1, j_2");
}

#[test]
fn each_orphaned_jobs_line_takes_and_opens_only_its_own_jobs() {
    let mut app = app();
    start(&mut app, 0);
    resumed(&mut app, true, 9_000);
    job_completed(&mut app, "j_1", Some("orphaned"));
    resumed(&mut app, true, 20_000);
    job_completed(&mut app, "j_2", Some("orphaned"));
    // A later orphan joins the latest line, not the first.
    job_completed(&mut app, "j_3", Some("orphaned"));
    assert_eq!(
        texts(&app),
        [
            "",
            "▄▄▄▄▄",
            " go ▐",
            "▀▀▀▀▀",
            "you 00:00 ",
            "",
            "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
            " ▣ cut short: Fiber stopped",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "↺ resumed",
            "Orphaned jobs: j_1",
            "Orphaned jobs: j_2, j_3",
        ]
    );
    // Opening the second line opens it alone.
    open(&mut app, "Orphaned jobs: j_2, j_3");
    assert_eq!(
        texts(&app)[10..],
        [
            "Orphaned jobs: j_1",
            "Orphaned jobs: j_2, j_3",
            "  j_2: j_2 orphaned.",
            "  j_3: j_3 orphaned.",
        ]
    );
}

#[test]
fn set_open_reports_whether_the_orphans_line_matched() {
    let is_open = |aside: &super::Aside| matches!(aside, super::Aside::Orphans { open: true, .. });
    let mut aside = super::Aside::Orphans {
        id: 7,
        jobs: vec![("job".to_owned(), "lost".to_owned())],
        open: false,
    };
    // The matching target sets the flag and answers true, both ways.
    assert!(aside.set_open(&Target::Orphans(7), true));
    assert!(is_open(&aside));
    assert!(aside.set_open(&Target::Orphans(7), false));
    assert!(!is_open(&aside));
    // Another line's id and another kind answer false and change nothing.
    assert!(!aside.set_open(&Target::Orphans(8), true));
    assert!(!is_open(&aside));
    assert!(!aside.set_open(&Target::Note(7), true));
    assert!(!is_open(&aside));
    // A plain line answers false too.
    let mut line = super::Aside::Line(ratatui::text::Line::raw("hi"));
    assert!(!line.set_open(&Target::Orphans(7), true));
}

/// A warning's rows and their texts at `inner` columns.
fn warning(message: &str, inner: u16) -> (Vec<String>, Vec<crate::rows::RowText>) {
    let mut out = crate::rows::Rows::default();
    super::Aside::Warning {
        message: message.to_owned(),
    }
    .rows(inner, &mut out);
    let (rows, texts) = out.into_parts();
    (
        rows.into_iter().map(|(line, _)| line.to_string()).collect(),
        texts,
    )
}

#[test]
fn a_warning_wraps_under_itself() {
    // A short message is one row.
    assert_eq!(
        warning("The MCP server one stopped.", 77).0,
        ["⚠ The MCP server one stopped."]
    );
    // A message of exactly `inner - 2` cells is one row, of `inner - 1`
    // cells is two, every continuation under the message's first column.
    let one = "a".repeat(18);
    assert_eq!(warning(&one, 20).0, [format!("⚠ {one}")]);
    let two = "a".repeat(19);
    assert_eq!(
        warning(&two, 20).0,
        [format!("⚠ {}", "a".repeat(18)), "  a".to_owned()]
    );
    let (rows, _) = warning("The MCP server one stopped working this morning", 20);
    assert_eq!(
        rows,
        [
            "⚠ The MCP server one",
            "  stopped working",
            "  this morning",
        ]
    );
    // A word longer than the width breaks hard.
    let (rows, _) = warning("aaaaaaaaaaaaaaaaaaaaa", 20);
    assert_eq!(rows, ["⚠ aaaaaaaaaaaaaaaaaa", "  aaa"]);
}

#[test]
fn a_warnings_copy_joins_with_a_space() {
    use crate::rows::Join;
    let (_, texts) = warning("The MCP server one stopped working", 20);
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0].join, Join::Break);
    assert_eq!(texts[0].skip, 2);
    assert_eq!(texts[1].join, Join::WrapSpace);
    assert_eq!(texts[1].skip, 2);
}

#[test]
fn a_warning_reads_its_mark_in_attention_and_its_message_dim() {
    use ratatui::style::Modifier;
    let mut out = crate::rows::Rows::default();
    super::Aside::Warning {
        message: "boom".to_owned(),
    }
    .rows(77, &mut out);
    let (rows, _) = out.into_parts();
    let spans = &rows[0].0.spans;
    assert_eq!(spans[0].content, "⚠ ");
    assert_eq!(
        spans[0].style,
        crate::markdown::style(crate::theme::Role::Attention)
    );
    assert_eq!(spans[1].content, "boom");
    assert!(spans[1].style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn set_open_and_toggle_ignore_a_warning() {
    let mut warning = super::Aside::Warning {
        message: "boom".to_owned(),
    };
    assert!(!warning.set_open(&Target::Orphans(7), true));
    assert_eq!(warning.toggle(Target::Orphans(7)), None);
}
