//! The accepted-import report overlay (`docs/tui.md`, "Notices",
//! "Overlays"): one scrollable overlay per accepted import, no notices.
//!
//! The ticket's screen test: a 120-line import answer opens one overlay
//! with both counts and no notice rows; scrolled to the bottom the last
//! row and the footer show; Esc closes it with no notices left behind.

use serde_json::{Value, json};

use ratatui::style::{Modifier, Style};

use contract::events::{ImportedNames, LeftBehindEntry};

use super::{ImportReport, PREFER, title_for};

use crate::app::commands::commands_testkit::{attached, enter, now, painted, sent, session_line};
use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use crate::view::overlay::{self, Row};

/// The 120-line import answer: 60 copied names across the 4 kinds and
/// 60 left-behind items, each one row in the report.
fn big_result() -> Value {
    let names = |prefix: &str| -> Vec<Value> {
        (1..=15)
            .map(|n| Value::String(format!("{prefix}-{n:02}")))
            .collect()
    };
    let left: Vec<Value> = (1..=60)
        .map(|n| {
            json!({"item": format!("left-{n:02}.json"), "reason": "credentials are never imported"})
        })
        .collect();
    json!({
        "imported": {
            "mcp_servers": names("server"),
            "skills": names("skill"),
            "commands": names("command"),
            "instructions": names("AGENTS.md"),
        },
        "left_behind": left,
    })
}

#[test]
fn an_accepted_import_with_120_lines_opens_one_report_and_adds_no_notices() {
    let mut app = attached();
    app.set_size(80, 24);
    let lines = sent(enter(&mut app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": big_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    // No notices: the report carries the whole answer. The report opens
    // scrolled to the top naming the source and its first section count;
    // the other count shows after scrolling down.
    assert!(
        app.notices().is_empty(),
        "an accepted import adds no notices"
    );
    let text = painted(&mut app, 80, 24).0;
    assert!(
        text.contains("Imported from Codex"),
        "the report names the source:\n{text}"
    );
    assert!(
        text.contains("Copied · 60"),
        "the copied section counts its items:\n{text}"
    );
    insta::assert_snapshot!("import_report_120_lines", text);
    let whole = painted(&mut app, 80, 140).0;
    assert!(
        whole.contains("Left behind · 60"),
        "the left-behind section counts its items:\n{whole}"
    );
    assert!(
        whole.contains("left-01.json") && whole.contains("left-60.json"),
        "the whole report shows at once:\n{whole}"
    );
    // Back at 80x24, scrolled to the bottom the last row shows with
    // the footer.
    app.set_size(80, 24);
    for _ in 0..30 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let bottom = painted(&mut app, 80, 24).0;
    assert!(
        bottom.contains("left-60.json"),
        "the last left-behind row shows at the bottom:\n{bottom}"
    );
    assert!(
        bottom.contains("Imported items apply on the next /reload · Esc closes"),
        "the footer stays visible at the bottom:\n{bottom}"
    );
    // Esc closes the overlay and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(
        app.notices().is_empty(),
        "closing the report leaves no notices"
    );
    let closed = painted(&mut app, 80, 24).0;
    assert!(
        !closed.contains("Imported from Codex"),
        "Esc closes the report:\n{closed}"
    );
}

/// The small report: 20 copied names and 3 left behind, 30 body rows,
/// 14 past the 16-row frame on an 80x24 screen.
fn small_result() -> Value {
    let names = |prefix: &str| -> Vec<Value> {
        (1..=5)
            .map(|n| Value::String(format!("{prefix}-{n:02}")))
            .collect()
    };
    json!({
        "imported": {
            "mcp_servers": names("server"),
            "skills": names("skill"),
            "commands": names("command"),
            "instructions": names("AGENTS.md"),
        },
        "left_behind": [
            {"item": "auth.json", "reason": "credentials are never imported"},
            {"item": "other.json", "reason": "unknown"},
            {"item": "third.json", "reason": "unknown"},
        ],
    })
}

/// The open report's scrolled-to first body row.
fn top_of(app: &App) -> usize {
    app.import_report().expect("open report").top()
}

/// Accepts `/import codex` with the small report on an attached app.
fn open_small_report(app: &mut App) {
    let lines = sent(enter(app, "/import codex"));
    let id = lines[0]["id"].clone();
    let accepted = session_line(
        "command_accepted",
        json!({"command_id": id, "result": small_result()}),
    );
    assert!(app.on_line(accepted).is_empty());
    assert!(app.import_report().is_some());
}

#[test]
fn scroll_keys_move_the_top_and_esc_closes_it() {
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    assert_eq!(top_of(&app), 0);
    // Up at the top holds.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(top_of(&app), 0);
    // Arrows move by rows, pages move further, and the bottom holds:
    // five pages reach the clamp however tall the conversation is.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 2);
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert!(top_of(&app) > 3, "a page moves past one row");
    let held = top_of(&app);
    for _ in 0..5 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let bottom = top_of(&app);
    assert!(bottom >= held, "pages only move down");
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(top_of(&app), bottom, "the bottom holds");
    // Pages back up reach the top exactly.
    for _ in 0..6 {
        assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    }
    assert_eq!(top_of(&app), 0);
    // A character key sends nothing and changes nothing: the draft
    // keeps nothing typed into the report.
    assert_eq!(app.on_key(Key::Char('x'), now()), Effect::None);
    assert_eq!(top_of(&app), 0);
    assert_eq!(app.draft(), "");
    assert!(app.import_report().is_some());
    // Esc closes the report and leaves no notices behind.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn ctrl_c_passes_through_to_quit_while_the_report_is_open() {
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::None);
    assert!(app.import_report().is_some());
    assert_eq!(app.on_key(Key::CtrlC, now()), Effect::Quit);
    assert!(app.import_report().is_some());
}

#[test]
fn wheel_scrolls_the_open_report_over_a_session() {
    use crate::keys::{Mouse, MouseKind};
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    let wheel = |down: bool| Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col: 40,
        row: 12,
    };
    app.on_wheel(&wheel(true));
    assert_eq!(top_of(&app), 3);
    app.on_wheel(&wheel(true));
    assert_eq!(top_of(&app), 6);
    app.on_wheel(&wheel(false));
    assert_eq!(top_of(&app), 3);
}

/// An app on home at 80x24, with no sessions.
fn home_app() -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// One live `session_status` line naming `name`.
fn status_line(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({
            "name": name, "workspace": "/w", "project": "p",
            "state": "idle", "since": 0, "started": 0, "calls": 0,
            "delegates": 0, "jobs": 0, "clients": 0,
            "model": "test/model",
            "spend": {"tokens": {"input": 0, "output": 0, "cache_read": 0, "cache_write": {}}, "cost": 0.0, "subscription_cost": 0.0},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// The small report opened straight onto `app`, naming `source`.
fn open_direct(app: &mut App, source: Option<&str>) {
    use contract::events::{ImportedNames, LeftBehindEntry};
    let result = small_result();
    let imported: ImportedNames =
        serde_json::from_value(result["imported"].clone()).expect("small report parses");
    let left_behind: Vec<LeftBehindEntry> =
        serde_json::from_value(result["left_behind"].clone()).expect("small report parses");
    app.open_import_report(source.map(str::to_owned), imported, left_behind);
}

#[test]
fn home_keys_route_to_the_report_while_it_is_open() {
    let mut app = home_app();
    open_direct(&mut app, Some("codex"));
    assert_eq!(
        app.import_report().expect("open report").title(),
        "Imported from Codex"
    );
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    // Typing reaches neither the box nor the list: the report keeps
    // the key.
    assert_eq!(app.on_key(Key::Char('j'), now()), Effect::None);
    assert_eq!(top_of(&app), 1);
    assert_eq!(app.draft(), "");
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
}

#[test]
fn wheel_routes_to_the_list_when_closed_and_the_report_when_open() {
    use crate::keys::{Mouse, MouseKind};
    let mut app = home_app();
    // A short screen, so the six sessions overflow the list's window.
    app.set_size(60, 12);
    for (at, name) in ["one", "two", "three", "four", "five", "six"]
        .iter()
        .enumerate()
    {
        let session = format!("s_000000000000000{at}");
        assert!(app.on_line(status_line(&session, name)).is_empty());
    }
    let wheel = |down: bool| Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col: 10,
        row: 5,
    };
    // With the report closed the wheel scrolls the session list.
    app.on_wheel(&wheel(true));
    assert_eq!(app.home_list_scroll(), 3);
    // With the report open the wheel scrolls the report instead, and
    // the list holds where it was.
    open_direct(&mut app, Some("codex"));
    app.on_wheel(&wheel(true));
    assert_eq!(app.home_list_scroll(), 3);
    assert_eq!(app.import_report().expect("open report").top(), 3);
}

#[test]
fn the_report_close_click_closes_the_report() {
    use crate::mouse::TargetId;
    let mut app = attached();
    app.set_size(80, 24);
    open_small_report(&mut app);
    let (_, targets) = painted(&mut app, 80, 24);
    let close = targets
        .iter()
        .find(|target| target.id == TargetId::CloseOverlay)
        .expect("the report draws its ✕")
        .id;
    assert_eq!(app.on_click(close), Effect::None);
    assert!(app.import_report().is_none());
    assert!(app.notices().is_empty());
    let closed = painted(&mut app, 80, 24).0;
    assert!(!closed.contains("Copied · 20"), "{closed}");
}

#[test]
fn home_paints_the_report_with_both_counts_and_a_swallow_target() {
    use crate::home::Spot;
    use crate::mouse::TargetId;
    let mut app = home_app();
    open_direct(&mut app, Some("codex"));
    let (shown, targets) = painted(&mut app, 80, 24);
    assert!(shown.contains("Imported from Codex"), "{shown}");
    assert!(shown.contains("Copied · 20"), "{shown}");
    assert!(shown.contains("Imported items apply"), "{shown}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Home(Spot::Question)),
        "clicks on the report stay with the report:\n{shown}"
    );
    assert!(app.notices().is_empty());
}

/// The report's unit tests: titles, sections, wrapping, scrolling
/// and framing, through the rows the screen test paints.
/// A report with every kind copied and two items left behind.
fn full() -> ImportReport {
    ImportReport::open(
        Some("codex"),
        ImportedNames {
            mcp_servers: vec!["zeta".to_owned(), "alpha".to_owned()],
            skills: vec!["lint".to_owned()],
            commands: vec!["plan".to_owned()],
            instructions: vec!["AGENTS.md".to_owned()],
        },
        vec![
            LeftBehindEntry {
                item: "auth.json".to_owned(),
                reason: "credentials are never imported".to_owned(),
            },
            LeftBehindEntry {
                item: "other.json".to_owned(),
                reason: "unknown".to_owned(),
            },
        ],
    )
}

#[test]
fn the_title_names_the_source() {
    assert_eq!(title_for(Some("claude-code")), "Imported from Claude Code");
    assert_eq!(title_for(Some("codex")), "Imported from Codex");
}

#[test]
fn a_bare_import_reports_the_generic_title() {
    assert_eq!(title_for(None), "Imported");
    assert_eq!(title_for(Some("prose")), "Imported");
}

/// The row texts, one per body row: styles assert separately.
fn texts(report: &ImportReport, inner: usize) -> Vec<String> {
    report
        .rows(inner)
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[test]
fn sections_group_the_copied_kinds_in_hub_order() {
    let texts = texts(&full(), 67);
    assert_eq!(
        texts,
        [
            "Copied · 5",
            "MCP servers",
            "zeta",
            "alpha",
            "Skills",
            "lint",
            "Commands",
            "plan",
            "Instructions",
            "AGENTS.md",
            "",
            "Left behind · 2",
            "auth.json: credentials are never imported",
            "other.json: unknown",
        ]
    );
}

#[test]
fn empty_groups_draw_no_header_but_kept_groups_keep_hub_order() {
    let report = ImportReport::open(
        Some("claude-code"),
        ImportedNames {
            skills: vec!["lint".to_owned()],
            ..ImportedNames::default()
        },
        Vec::new(),
    );
    assert_eq!(
        texts(&report, 67),
        ["Copied · 1", "Skills", "lint", "", "Left behind · 0"]
    );
}

#[test]
fn an_empty_report_draws_both_sections_with_zero_counts() {
    let report = ImportReport::open(None, ImportedNames::default(), Vec::new());
    assert_eq!(report.title(), "Imported");
    assert_eq!(texts(&report, 67), ["Copied · 0", "", "Left behind · 0"]);
}

#[test]
fn section_counts_are_item_counts() {
    let report = full();
    assert_eq!(report.copied_count(), 5);
    assert_eq!(report.left_count(), 2);
}

#[test]
fn a_left_behind_row_carries_its_reason_dim() {
    let rows = full().rows(67);
    let row = &rows[12];
    assert_eq!(row.spans.len(), 2);
    assert_eq!(row.spans[0].content, "auth.json: ");
    assert_eq!(row.spans[0].style, Style::new());
    assert_eq!(row.spans[1].content, "credentials are never imported");
    assert_eq!(row.spans[1].style, Style::new().add_modifier(Modifier::DIM));
}

#[test]
fn a_wrapped_left_behind_row_keeps_the_reason_dim_past_the_break() {
    let report = ImportReport::open(
        None,
        ImportedNames::default(),
        vec![LeftBehindEntry {
            item: "auth.json".to_owned(),
            reason: "credentials are never imported anywhere".to_owned(),
        }],
    );
    let rows = report.rows(30);
    let plain: Vec<String> = rows
        .iter()
        .skip(3)
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect();
    assert_eq!(
        plain,
        ["auth.json: credentials are", "never imported anywhere",]
    );
    // The break falls inside the reason, so the first row's tail
    // and the whole second row read dim.
    assert_eq!(rows[3].spans.len(), 2);
    assert_eq!(
        rows[3].spans[1].style,
        Style::new().add_modifier(Modifier::DIM)
    );
    assert_eq!(rows[4].spans.len(), 1);
    assert_eq!(
        rows[4].spans[0].style,
        Style::new().add_modifier(Modifier::DIM)
    );
}

#[test]
fn a_row_spanning_the_item_reason_boundary_counts_the_dropped_space_with_the_item() {
    // The item's word runs past the row, so the middle row spans
    // the `item: reason` boundary: the wrap drops the boundary's
    // space, and the `WrapSpace` offset counts it on the item
    // side. Without the offset the boundary moves one cell into
    // the item, dimming its trailing space and shifting the
    // reason's start.
    let report = ImportReport::open(
        None,
        ImportedNames::default(),
        vec![LeftBehindEntry {
            item: "abcdefghijklmnopqrstuv".to_owned(),
            reason: "why not".to_owned(),
        }],
    );
    let rows = report.rows(14);
    assert_eq!(rows.len(), 6);
    let row = &rows[4];
    assert_eq!(row.spans.len(), 2);
    assert_eq!(row.spans[0].content, "opqrstuv: ");
    assert_eq!(row.spans[0].style, Style::new());
    assert_eq!(row.spans[1].content, "why");
    assert_eq!(row.spans[1].style, Style::new().add_modifier(Modifier::DIM));
}

#[test]
fn scrolling_by_rows_holds_at_both_edges() {
    let report = full();
    // Fourteen body rows in a two-row frame scroll twelve.
    let max = report
        .rows(67)
        .len()
        .saturating_sub(overlay::question_capacity(10));
    assert_eq!(max, 12);
    let mut scrolled = report.clone();
    scrolled.scroll_by(true, 1, max);
    assert_eq!(scrolled.top(), 1);
    scrolled.scroll_by(false, 1, max);
    assert_eq!(scrolled.top(), 0);
    // Up at the top holds.
    scrolled.scroll_by(false, 5, max);
    assert_eq!(scrolled.top(), 0);
    // Down past the end holds at the last row the frame shows.
    scrolled.scroll_by(true, max.saturating_add(10), max);
    assert_eq!(scrolled.top(), max);
    scrolled.scroll_by(true, 1, max);
    assert_eq!(scrolled.top(), max);
    // One step back from the end moves again.
    scrolled.scroll_by(false, 1, max);
    assert_eq!(scrolled.top(), max.saturating_sub(1));
}

#[test]
fn scrolling_clamps_a_stale_offset_before_it_moves() {
    let mut report = full();
    let max = report
        .rows(67)
        .len()
        .saturating_sub(overlay::question_capacity(10));
    report.top = max.saturating_add(4);
    report.scroll_by(true, 1, max);
    assert_eq!(report.top(), max);
    report.top = max.saturating_add(4);
    report.scroll_by(false, 1, max);
    assert_eq!(report.top(), max.saturating_sub(1));
}

#[test]
fn paging_moves_a_page_and_holds_at_the_edges() {
    let report = full();
    // Fourteen body rows in a two-row frame: a page is two rows.
    let max = report
        .rows(67)
        .len()
        .saturating_sub(overlay::question_capacity(10));
    let page = overlay::question_capacity(10);
    assert_eq!((max, page), (12, 2));
    let mut scrolled = report.clone();
    scrolled.scroll_page(true, 10, max);
    assert_eq!(scrolled.top(), page.min(max));
    scrolled.scroll_page(true, 10, max);
    scrolled.scroll_page(true, 10, max);
    scrolled.scroll_page(true, 10, max);
    scrolled.scroll_page(true, 10, max);
    scrolled.scroll_page(true, 10, max);
    assert_eq!(scrolled.top(), max);
    scrolled.scroll_page(true, 10, max);
    assert_eq!(scrolled.top(), max);
    scrolled.scroll_page(false, 10, max);
    assert_eq!(scrolled.top(), max.saturating_sub(page));
    let mut top = report.clone();
    top.scroll_page(false, 10, max);
    assert_eq!(top.top(), 0);
}

#[test]
fn the_frame_windows_the_body_at_the_scrolled_offset() {
    let mut report = full();
    let inner = 67;
    let height = 12;
    let cap = overlay::question_capacity(height);
    let all = texts(&report, inner);
    assert!(all.len() > cap);
    let framed = report.frame(inner, height);
    assert_eq!(
        framed.title.as_ref().map(|title| title.0.clone()),
        Some("Imported from Codex".to_owned())
    );
    assert_eq!(framed.prefer, PREFER);
    assert!(framed.footer.is_some());
    let shown: Vec<String> = framed.body.iter().map(text_of).collect();
    assert_eq!(shown, all[..cap]);
    // Scrolled to the bottom the frame shows the last rows.
    let max = report.max_top(inner, height);
    report.top = max;
    let scrolled = report.frame(inner, height);
    let shown: Vec<String> = scrolled.body.iter().map(text_of).collect();
    assert_eq!(shown, all[max..]);
    assert!(shown.contains(&"other.json: unknown".to_owned()));
}

/// One body row's text.
fn text_of(row: &Row) -> String {
    row.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}
