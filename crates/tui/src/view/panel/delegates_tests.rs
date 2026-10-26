//! Tests for the Delegates card: which delegates it lists and their rows.

use super::DELEGATES_SHOWN;
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use crate::view::panel::{Card, cards, rows};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// Names as a card list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// An app with home state, attached, at `width` by `height`, drawing
/// `names`.
fn attached_with(width: u16, height: u16, names: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: list(names),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// An app drawing the default card list.
fn attached(width: u16, height: u16) -> App {
    attached_with(width, height, &CARDS)
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds a started job `job` of `description`.
fn fold_job(app: &mut App, job: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        serde_json::json!({"job_id": job, "description": description,
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a delegate on `harness` with `model` as job `job`.
fn fold_delegate(app: &mut App, job: &str, harness: &str, model: &str, description: &str) {
    fold_job(app, job, description);
    app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": format!("s_{job:0>16}"),
            "harness": harness, "model": model, "workspace": "/w"}),
    ));
}

/// Folds the end of delegate job `job`: `delegate_finished`, then
/// `job_completed`.
fn finish(app: &mut App, job: &str) {
    app.on_line(session_line(
        "delegate_finished",
        serde_json::json!({"job_id": job, "text": "done",
            "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": null, "subscription_cost": 0.0}}),
    ));
    app.on_line(session_line(
        "job_completed",
        serde_json::json!({"job_id": job, "status": "completed"}),
    ));
}

/// Folds Fiber delegate `j_<k>`, described `task <k>`.
fn fold_fiber(app: &mut App, k: usize) {
    fold_delegate(
        app,
        &format!("j_{k}"),
        "fiber",
        "test/model",
        &format!("task {k}"),
    );
}

/// Folds `n` Fiber delegates, `j_1` to `j_<n>`.
fn fold_delegates(app: &mut App, n: usize) {
    for k in 1..=n {
        fold_fiber(app, k);
    }
}

/// The panel rect's width.
fn panel_width(app: &App) -> u16 {
    app.chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"))
        .width
}

/// The text width of the panel's cards: the slab offset and the two
/// columns of padding on the left and one on the right.
fn text_width(app: &App) -> usize {
    usize::from(panel_width(app)).saturating_sub(4)
}

/// The Delegates card's running-only rows' text: the narrow layout's.
fn card_texts(app: &App) -> Vec<String> {
    super::rows(app, text_width(app))
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// The Delegates card's combined rows' text: running first, then done.
fn done_texts(app: &App) -> Vec<String> {
    super::rows_with_done(app, text_width(app))
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// Every panel row's text.
fn texts(app: &App) -> Vec<String> {
    rows(app, panel_width(app))
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// Draws only the panel rect of `app`, with its click targets.
fn draw_targets(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    crate::view::panel::draw(app, area, &mut buf, &mut targets);
    (buf, targets)
}

#[test]
fn a_running_delegate_shows_its_description_with_its_state_below() {
    let mut app = attached(160, 40);
    fold_delegate(&mut app, "j_1", "fiber", "test/model", "review the parser");
    // The running-only rows are the narrow layout's.
    assert_eq!(
        card_texts(&app),
        vec![
            "\u{25cf} review the parser".to_owned(),
            "  fiber \u{00b7} test/model".to_owned()
        ]
    );
    // Once it completes the narrow rows go, and the card keeps it as
    // done.
    finish(&mut app, "j_1");
    assert!(card_texts(&app).is_empty());
    assert_eq!(
        done_texts(&app),
        vec!["\u{2713} review the parser".to_owned(), "  done".to_owned()]
    );
}

#[test]
fn a_job_without_delegate_started_is_not_a_delegate() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    assert!(card_texts(&app).is_empty());
    assert!(app.panel_state().running_delegates().is_empty());
}

#[test]
fn without_a_status_the_rows_show_the_description_with_harness_and_model_below() {
    let mut app = attached(160, 40);
    fold_delegate(&mut app, "j_1", "claude", "opus", "write the docs");
    let drawn = super::rows(&app, text_width(&app));
    assert_eq!(drawn.len(), 2);
    let first = &drawn[0];
    assert_eq!(first.line.to_string(), "\u{25cf} write the docs");
    let glyph = first
        .line
        .spans
        .first()
        .unwrap_or_else(|| panic!("a glyph span"));
    assert_eq!(glyph.content, "\u{25cf}");
    assert_eq!(
        glyph.style,
        crate::theme::style(crate::theme::Role::Attention)
    );
    let second = &drawn[1];
    assert_eq!(second.line.to_string(), "  claude \u{00b7} opus");
    assert_eq!(
        second.line.spans.first().map(|span| span.style),
        Some(crate::theme::style(crate::theme::Role::Muted))
    );
}

#[test]
fn three_delegates_show_and_a_fourth_does_not() {
    assert_eq!(DELEGATES_SHOWN, 3);
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 3);
    let three = card_texts(&app);
    assert_eq!(three.len(), 6);
    assert_eq!(three.get(4).map(String::as_str), Some("\u{25cf} task 3"));
    assert_eq!(
        three.get(5).map(String::as_str),
        Some("  fiber \u{00b7} test/model")
    );
    fold_fiber(&mut app, 4);
    let four = card_texts(&app);
    assert_eq!(four, three);
    assert!(!texts(&app).iter().any(|row| row == "  task 4"));
}

#[test]
fn the_description_is_cut_at_the_text_width() {
    let mut app = attached(160, 40);
    let text = text_width(&app);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "test/model",
        &"x".repeat(text + 10),
    );
    // The description rides the first row, cut with `…` past the card.
    let drawn = card_texts(&app);
    let first = drawn.first().unwrap_or_else(|| panic!("a first row"));
    assert_eq!(crate::format::width(first), text);
    assert_eq!(*first, format!("\u{25cf} {}\u{2026}", "x".repeat(text - 3)));
}

#[test]
fn both_rows_of_each_delegate_carry_its_serial_as_their_target() {
    use crate::app::panel::Spot;
    use crate::mouse::TargetId;
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 2);
    let rows = super::rows(&app, text_width(&app));
    assert_eq!(rows.len(), 4);
    let spots: Vec<_> = rows.iter().map(|row| row.spot).collect();
    assert_eq!(
        spots,
        vec![
            Some(Spot::Delegate(1)),
            Some(Spot::Delegate(1)),
            Some(Spot::Delegate(2)),
            Some(Spot::Delegate(2)),
        ]
    );
    let (_, targets) = draw_targets(&app);
    let drawn: Vec<_> = targets
        .iter()
        .filter_map(|target| {
            if let TargetId::Panel(Spot::Delegate(serial)) = target.id {
                Some(serial)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(drawn, vec![1, 1, 2, 2]);
}

#[test]
fn delegates_places_one_card() {
    let none: [(&str, &str); 0] = [];
    assert_eq!(
        cards(&list(&["delegates", "delegates"]), &none),
        vec![Card::Delegates]
    );
}

#[test]
fn the_card_takes_its_listed_place() {
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["jobs", "delegates", "session"]), &widgets),
        vec![Card::Jobs, Card::Delegates, Card::Session, Card::Widget(0)]
    );
    let mut app = attached_with(160, 40, &["jobs", "delegates"]);
    fold_job(&mut app, "j_0", "build");
    fold_delegates(&mut app, 1);
    let edge = "\u{2584}".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let edge_bottom = "\u{2580}".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let text = text_width(&app);
    let jobs = format!(
        "Jobs{}1 running \u{25b8}",
        " ".repeat(text - "Jobs".len() - crate::format::width("1 running \u{25b8}"))
    );
    let delegates = format!(
        "Delegates{}1 running",
        " ".repeat(text - "Delegates".len() - "1 running".len())
    );
    assert_eq!(
        texts(&app),
        vec![
            edge.clone(),
            jobs,
            edge_bottom.clone(),
            String::new(),
            edge,
            delegates,
            "\u{25cf} task 1".to_owned(),
            "  fiber \u{00b7} test/model".to_owned(),
            edge_bottom,
        ]
    );
}

#[test]
fn no_delegates_draws_no_card_and_no_blank_row() {
    let mut app = attached_with(160, 40, &["delegates", "jobs"]);
    fold_job(&mut app, "j_0", "build");
    let edge = "\u{2584}".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let edge_bottom = "\u{2580}".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let text = text_width(&app);
    let jobs = format!(
        "Jobs{}1 running \u{25b8}",
        " ".repeat(text - "Jobs".len() - crate::format::width("1 running \u{25b8}"))
    );
    assert_eq!(texts(&app), vec![edge, jobs, edge_bottom]);
}

#[test]
fn delegates_card_from_the_fold() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_0", "build the workspace");
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "anthropic/opus",
        "review the parser",
    );
    fold_delegate(&mut app, "j_2", "claude", "sonnet", "write the docs");
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_from_the_fold", crate::view::text(&buf));
}

#[test]
fn delegates_card_at_the_panel_floor() {
    let mut app = attached(140, 24);
    assert_eq!(panel_width(&app), 30);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "anthropic/claude-opus-5-5-with-a-long-name",
        "review the parser and the lexer together",
    );
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_at_the_panel_floor", crate::view::text(&buf));
}

/// Scrolls the card down `steps` delegates with the wheel over its first
/// row.
fn scroll_card(app: &mut App, steps: usize) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    for _ in 0..steps {
        app.on_wheel(&crate::keys::Mouse {
            kind: crate::keys::MouseKind::WheelDown,
            col: rect.x.saturating_add(4),
            row: rect.y.saturating_add(1),
        });
    }
}

#[test]
fn the_card_shows_from_its_scroll_offset() {
    let mut app = attached_with(160, 40, &["delegates"]);
    fold_delegates(&mut app, 5);
    scroll_card(&mut app, 2);
    assert_eq!(app.panel_state().delegate_scroll(), 2);
    let firsts: Vec<String> = card_texts(&app).into_iter().step_by(2).collect();
    assert_eq!(
        firsts,
        vec!["\u{25cf} task 3", "\u{25cf} task 4", "\u{25cf} task 5"]
    );
    // Two delegates finish while scrolled: they stay on the card after
    // the running ones, and the narrow rows keep the running ones only.
    for job in ["j_4", "j_5"] {
        app.on_line(session_line(
            "job_completed",
            serde_json::json!({"job_id": job, "status": "completed"}),
        ));
    }
    assert_eq!(
        card_texts(&app),
        vec![
            "\u{25cf} task 3".to_owned(),
            "  fiber \u{00b7} test/model".to_owned()
        ]
    );
    assert_eq!(
        done_texts(&app),
        vec![
            "\u{25cf} task 3".to_owned(),
            "  fiber \u{00b7} test/model".to_owned(),
            "\u{2713} task 4".to_owned(),
            "  done".to_owned(),
            "\u{2713} task 5".to_owned(),
            "  done".to_owned(),
        ]
    );
}

#[test]
fn the_span_covers_only_the_card_rows() {
    let mut app = attached_with(160, 40, &["session", "delegates", "jobs"]);
    app.on_line(session_line(
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 2, "jobs": 3, "clients": 0,
        }),
    ));
    fold_job(&mut app, "j_0", "build");
    fold_delegates(&mut app, 2);
    let (drawn, span) = crate::view::panel::rows_and_delegates(&app, panel_width(&app));
    let texts: Vec<String> = drawn.iter().map(|row| row.line.to_string()).collect();
    let start = texts
        .iter()
        .position(|row| row == "\u{25cf} task 1")
        .unwrap_or_else(|| panic!("the card's first row"));
    assert!(start > 2);
    // The edge, the title row, then the content: the span covers the
    // card's rows from its top edge to its bottom edge.
    assert_eq!(span, Some(start - 2..start + 5));
    assert!(texts[start - 1].starts_with("Delegates"));
    assert_eq!(texts.get(start - 3).map(String::as_str), Some(""));
    assert_eq!(texts.get(start + 5).map(String::as_str), Some(""));
    assert!(
        texts
            .get(start + 6)
            .is_some_and(|row| row.starts_with('\u{2584}')),
        "{texts:?}"
    );
    assert!(
        texts
            .get(start + 7)
            .is_some_and(|row| row.ends_with("1 running \u{25b8}")),
        "{texts:?}"
    );
}

#[test]
fn delegates_card_scrolled() {
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 5);
    scroll_card(&mut app, 1);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_scrolled", crate::view::text(&buf));
}

/// A delegate's `session_status` in `state`, naming the attached session.
fn delegate_status(app: &mut App, job: &str, state: serde_json::Value) {
    let mut payload = serde_json::json!({
        "name": "delegate one", "workspace": "/w", "project": "-w",
        "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "status/model", "delegates": 0, "jobs": 0, "clients": 0,
        "parent": SESSION,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(format!("s_{job:0>16}")),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

/// A delegate's unreadable `session_status`: a schema this terminal does
/// not read. Its payload still names the parent, as the hub's does.
fn unreadable_status(app: &mut App, job: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(format!("s_{job:0>16}")),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION + 1,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({"parent": SESSION})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
}

fn session_line_at(kind: &str, ts: u64, payload: serde_json::Value) -> Line {
    let Line::Session(mut line) = session_line(kind, payload) else {
        unreachable!()
    };
    line.ts = ts;
    Line::Session(line)
}

#[test]
fn progress_running_rows_show_calls_elapsed_and_the_optional_step() {
    let clock = fakes::clock::FakeClock::new();
    const WALL: u64 = 1_700_000_000_000;
    for (fields, expected) in [
        (
            serde_json::json!({"calls": 14, "started": WALL-130_000, "step": "read app.rs"}),
            "  14 calls · 2m 10s · read app.rs",
        ),
        (
            serde_json::json!({"calls": 1, "started": WALL-130_000}),
            "  1 call · 2m 10s",
        ),
        (
            serde_json::json!({"calls": 0, "started": WALL-3_000}),
            "  0 calls · 3s",
        ),
        (
            serde_json::json!({"calls": 2, "started": WALL+1}),
            "  2 calls · 0s",
        ),
    ] {
        let mut app = attached(160, 40);
        app.set_now(clock.origin(), WALL);
        fold_fiber(&mut app, 1);
        let mut state = fields;
        state["state"] = serde_json::json!("idle");
        delegate_status(&mut app, "j_1", state);
        let drawn = super::rows(&app, 100);
        assert_eq!(drawn[1].line.to_string(), expected);
        assert_eq!(
            drawn[1].line.spans[0].style,
            crate::theme::style(crate::theme::Role::Muted)
        );
    }
    let mut app = attached(160, 40);
    fold_fiber(&mut app, 1);
    assert_eq!(
        super::rows(&app, 100)[1].line.to_string(),
        "  fiber · test/model"
    );
    unreadable_status(&mut app, "j_1");
    assert!(
        super::rows(&app, 100)[1]
            .line
            .to_string()
            .contains("CANNOT ATTACH")
    );
}

#[test]
fn progress_finished_rows_freeze_the_last_status_at_the_first_end() {
    for (outcome, word, with_finish, elapsed) in [
        ("completed", "done", true, "48s"),
        ("completed", "done", false, "58s"),
        ("failed", "failed", true, "48s"),
        ("cancelled", "cancelled", true, "48s"),
    ] {
        let mut app = attached(160, 40);
        fold_fiber(&mut app, 1);
        delegate_status(
            &mut app,
            "j_1",
            serde_json::json!({"state": "idle", "calls": 6, "started": 2000}),
        );
        if with_finish {
            app.on_line(session_line_at("delegate_finished", 50_000, serde_json::json!({"job_id": "j_1", "text": "done",
                "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0}, "cost": null, "subscription_cost": 0.0}})));
        }
        app.on_line(session_line_at(
            "job_completed",
            60_000,
            serde_json::json!({"job_id": "j_1", "status": outcome}),
        ));
        assert_eq!(
            super::rows_with_done(&app, 100)[1].line.to_string(),
            format!("  6 calls · {elapsed} · {word}")
        );
        assert!(
            app.delegate_row(&contract::SessionId(format!("s_{:0>16}", "j_1")))
                .is_none()
        );
        // A resumed job must not reuse either the status or its ending.
        fold_job(&mut app, "j_1", "again");
        app.on_line(session_line_at(
            "job_completed",
            70_000,
            serde_json::json!({"job_id": "j_1", "status": "completed"}),
        ));
        assert_eq!(
            super::rows_with_done(&app, 100)[1].line.to_string(),
            "  done"
        );
    }
}

#[test]
fn with_a_status_the_second_row_shows_its_progress() {
    use crate::theme::Role;
    // The first row keeps the state glyph; progress is dim below it.
    for (state, _word) in [
        (serde_json::json!({"state": "streaming"}), "WORKING"),
        (
            serde_json::json!({"state": "tool", "tool": "read"}),
            "WORKING",
        ),
        (serde_json::json!({"state": "retrying"}), "RETRYING"),
        (serde_json::json!({"state": "jobs"}), "WORKING"),
        (serde_json::json!({"state": "idle"}), "READY"),
    ] {
        let mut app = attached(160, 40);
        fold_fiber(&mut app, 1);
        delegate_status(&mut app, "j_1", state);
        let drawn = super::rows(&app, text_width(&app));
        assert_eq!(drawn.len(), 2);
        let first = &drawn[0];
        assert_eq!(first.line.to_string(), "\u{25cf} task 1");
        let glyph = first
            .line
            .spans
            .first()
            .unwrap_or_else(|| panic!("a glyph span"));
        assert_eq!(glyph.content, "\u{25cf}");
        assert_eq!(glyph.style, crate::theme::style(Role::Attention));
        let second = &drawn[1];
        assert_eq!(second.line.to_string(), "  0 calls · 0s");
        assert_eq!(
            second.line.spans.first().map(|span| span.style),
            Some(crate::theme::style(Role::Muted))
        );
    }
    // An unreadable status still runs: `●` with its word below.
    let mut app = attached(160, 40);
    fold_fiber(&mut app, 1);
    unreadable_status(&mut app, "j_1");
    let drawn = super::rows(&app, text_width(&app));
    assert_eq!(drawn[0].line.to_string(), "\u{25cf} task 1");
    assert!(
        drawn[1].line.to_string().contains("CANNOT ATTACH"),
        "{}",
        drawn[1].line
    );
}

#[test]
fn the_progress_row_does_not_show_the_model() {
    let mut app = attached(160, 40);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "started/model",
        "review the parser",
    );
    delegate_status(&mut app, "j_1", serde_json::json!({"state": "streaming"}));
    let drawn = super::rows(&app, text_width(&app));
    // The description remains above the progress, regardless of model.
    assert_eq!(drawn[0].line.to_string(), "\u{25cf} review the parser");
    assert_eq!(drawn[1].line.to_string(), "  0 calls · 0s");
}

/// A delegate's end draws from its outcome: the glyph, the description
/// and a dim word. Attention is never used for a finished delegate.
#[test]
fn finished_delegates_show_how_they_ended() {
    use crate::theme::Role;
    for (status, glyph, tone, word) in [
        ("completed", "\u{2713}", Role::Success, "done"),
        ("failed", "\u{2717}", Role::Muted, "failed"),
        ("cancelled", "\u{2298}", Role::Muted, "cancelled"),
    ] {
        let mut app = attached(160, 40);
        fold_fiber(&mut app, 1);
        app.on_line(session_line(
            "job_completed",
            serde_json::json!({"job_id": "j_1", "status": status}),
        ));
        let drawn = super::rows_with_done(&app, text_width(&app));
        assert_eq!(drawn.len(), 2, "{status}");
        let first = &drawn[0];
        assert_eq!(
            first.line.to_string(),
            format!("{glyph} task 1"),
            "{status}"
        );
        let mark = first
            .line
            .spans
            .first()
            .unwrap_or_else(|| panic!("a glyph span"));
        assert_eq!(mark.content, glyph, "{status}");
        assert_eq!(mark.style, crate::theme::style(tone), "{status}");
        let second = &drawn[1];
        assert_eq!(second.line.to_string(), format!("  {word}"), "{status}");
        assert_eq!(
            second.line.spans.first().map(|span| span.style),
            Some(crate::theme::style(Role::Muted)),
            "{status}"
        );
        for row in &drawn {
            for span in &row.line.spans {
                assert_ne!(span.style, crate::theme::style(Role::Attention), "{status}");
            }
        }
    }
}

/// The title counts the running delegates only: attention while one
/// runs, dim with only finished ones held. A card of only finished
/// delegates still draws.
#[test]
fn the_title_counts_only_running() {
    use crate::theme::Role;
    // One running, one done: the figure counts one, in attention.
    let mut app = attached_with(160, 40, &["delegates"]);
    fold_fiber(&mut app, 1);
    fold_fiber(&mut app, 2);
    finish(&mut app, "j_1");
    let drawn = texts(&app);
    assert_eq!(drawn.len(), 7);
    let title = drawn.get(1).expect("a title row");
    assert!(title.ends_with("1 running"), "{title}");
    let rows = crate::view::panel::rows(&app, panel_width(&app));
    assert_eq!(
        rows[1].line.spans.last().map(|span| span.style),
        Some(crate::theme::style(Role::Attention))
    );
    // Only finished ones held: the card draws with a dim `0 running`.
    let mut app = attached_with(160, 40, &["delegates"]);
    fold_fiber(&mut app, 1);
    finish(&mut app, "j_1");
    let drawn = texts(&app);
    assert_eq!(drawn.len(), 5);
    assert!(drawn[1].ends_with("0 running"), "{drawn:?}");
    assert_eq!(done_texts(&app), vec!["\u{2713} task 1", "  done"]);
    let rows = crate::view::panel::rows(&app, panel_width(&app));
    assert_eq!(
        rows[1].line.spans.last().map(|span| span.style),
        Some(crate::theme::style(Role::Muted))
    );
}

/// Running delegates come first, so a new running delegate is never
/// behind finished ones; the narrow rows stay running-only.
#[test]
fn a_new_running_delegate_shows_before_finished_ones() {
    let mut app = attached_with(160, 40, &["delegates"]);
    fold_fiber(&mut app, 1);
    finish(&mut app, "j_1");
    fold_fiber(&mut app, 2);
    assert_eq!(
        done_texts(&app),
        vec![
            "\u{25cf} task 2".to_owned(),
            "  fiber \u{00b7} test/model".to_owned(),
            "\u{2713} task 1".to_owned(),
            "  done".to_owned(),
        ]
    );
    assert_eq!(
        card_texts(&app),
        vec![
            "\u{25cf} task 2".to_owned(),
            "  fiber \u{00b7} test/model".to_owned()
        ]
    );
}

#[test]
fn delegates_card_with_statuses() {
    let mut app = attached(160, 40);
    fold_fiber(&mut app, 1);
    delegate_status(&mut app, "j_1", serde_json::json!({"state": "streaming"}));
    fold_delegate(&mut app, "j_2", "claude", "sonnet", "write the docs");
    fold_fiber(&mut app, 3);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_with_statuses", crate::view::text(&buf));
}
