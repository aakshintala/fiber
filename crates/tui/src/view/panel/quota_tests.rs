//! The card's names, figures, colours and bars, through a quota answer.
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use serde_json::{Value, json};

use super::{rows, segment_spans};
use crate::app::App;
use crate::app::items::testkit::{SESSION, commands, hello, session_line};
use crate::app::panel::Spot;
use crate::home::Launch;
use crate::theme::{Role, style};

fn app(quota: Value, notice_at: f64) -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_home(Launch {
        panel_cards: vec!["quota".to_owned()],
        ..Default::default()
    });
    app.on_line(hello());
    app.attach(contract::SessionId(SESSION.to_owned()));
    let out = commands(app.on_line(session_line("clients", json!({"clients": []}))));
    let id = out.iter().find(|v| v["command"] == "quota").unwrap()["id"]
        .as_str()
        .unwrap();
    app.on_line(session_line(
        "command_accepted",
        json!({"command_id": id,
        "result": {"quota": quota, "notice_at": notice_at}}),
    ));
    app
}

fn window(used: f64) -> Value {
    json!([{"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "percent_used": used}]}])
}

fn text(rows: &[super::super::Row]) -> Vec<String> {
    rows.iter().map(|r| r.line.to_string()).collect()
}

#[test]
fn window_share_is_left_and_floors_at_zero() {
    for (used, figure) in [(35.0, "65%"), (99.0, "1%"), (100.0, "0%"), (101.0, "0%")] {
        let rows = rows(&app(window(used), 80.0), 30);
        assert_eq!(text(&rows)[1], format!("codex weekly{:>18}", figure));
        // The title and the data row open the quota view; the bar opens nothing.
        assert_eq!(rows[0].spot, Some(Spot::Quota));
        assert_eq!(rows[1].spot, Some(Spot::Quota));
        assert!(rows[2].spot.is_none());
    }
}

#[test]
fn threshold_colours_the_figure_and_bar_at_and_above_notice_at() {
    for (used, expected) in [
        (69.9, Role::Accent),
        (70.0, Role::Error),
        (70.1, Role::Error),
    ] {
        let rows = rows(&app(window(used), 70.0), 30);
        let figure = rows[1].line.spans.last().unwrap();
        assert_eq!(
            figure.style,
            style(if used < 70.0 { Role::Text } else { Role::Error })
        );
        assert_eq!(rows[2].line.spans[0].style, style(expected));
    }
}

#[test]
fn bar_rounds_and_keeps_one_cell_only_for_a_positive_share() {
    for (used, filled) in [(100.0, 0), (99.6, 1), (50.0, 5), (0.0, 10)] {
        let rows = rows(&app(window(used), 101.0), 12);
        let spans = &rows[2].line.spans;
        assert_eq!(
            spans
                .iter()
                .filter(|s| s.style == style(Role::Accent))
                .map(|s| s.content.chars().count())
                .sum::<usize>(),
            filled
        );
        assert_eq!(rows[2].line.width(), 10);
    }
    assert_eq!(rows(&app(window(50.0), 80.0), 1)[2].line.width(), 0);
}

#[test]
fn labels_and_windows_have_unambiguous_names() {
    let app = app(
        json!([
            {"provider": "codex", "credential": "home", "windows": [{"name": "weekly", "percent_used": 35.0}]},
            {"provider": "codex", "credential": "work", "windows": [{"name": "primary", "percent_used": 60.0}, {"name": "secondary", "reset": true}]},
            {"provider": "openrouter", "credential": "work", "credit": {"remaining": 4.2}}
        ]),
        80.0,
    );
    let shown = text(&rows(&app, 40));
    assert!(shown[1].starts_with("codex home weekly"));
    assert!(shown[3].starts_with("codex work primary"));
    assert!(shown[5].starts_with("codex work secondary"));
    assert!(shown[7].starts_with("openrouter credit"));
    assert_eq!(
        segment_spans(&app)
            .unwrap()
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>(),
        "Q codex home 65% · codex work primary 40% · codex work secondary reset · openrouter 4.20"
    );
}

#[test]
fn credit_shares_clamp_and_zero_or_missing_limits_have_no_bar_or_error() {
    for (remaining, limit, figure, count, error) in [
        (4.2, Some(10.0), "42%", 3, false),
        (2.01, Some(10.0), "20%", 3, false),
        (2.0, Some(10.0), "20%", 3, true),
        (1.99, Some(10.0), "20%", 3, true),
        (12.0, Some(10.0), "100%", 3, false),
        (-1.0, Some(10.0), "0%", 3, true),
        (4.2, None, "4.20", 2, false),
        (-1.0, None, "-1.00", 2, false),
        (4.2, Some(0.0), "4.20", 2, false),
        (4.2, Some(-0.1), "4.20", 2, false),
        (0.0, Some(0.1), "0%", 3, true),
    ] {
        let mut credit = json!({"remaining": remaining});
        if let Some(limit) = limit {
            credit["limit"] = limit.into();
        }
        let rows = rows(
            &app(
                json!([{"provider": "openrouter", "credential": "default", "credit": credit}]),
                80.0,
            ),
            30,
        );
        assert_eq!(rows.len(), count);
        let last = rows[1].line.spans.last().unwrap();
        assert_eq!(last.content, figure);
        assert_eq!(
            last.style,
            style(if error { Role::Error } else { Role::Text })
        );
    }
}

#[test]
fn reset_is_full_accent_and_unreachable_keeps_only_a_last_value() {
    let app = app(
        json!([
            {"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "reset": true}], "unreachable": true},
            {"provider": "missing", "credential": "work", "unreachable": true}
        ]),
        0.0,
    );
    let rows = rows(&app, 30);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].line.spans.last().unwrap().content, "reset");
    assert_eq!(rows[1].line.spans.last().unwrap().style, style(Role::Text));
    assert_eq!(rows[2].line.spans[0].content.chars().count(), 28);
    assert_eq!(rows[2].line.spans[0].style, style(Role::Accent));
    assert_eq!(
        segment_spans(&app)
            .unwrap()
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>(),
        "Q codex reset"
    );
}

#[test]
fn empty_quota_has_no_card_slab_title_or_segment() {
    for quota in [
        json!([]),
        json!([{"provider": "codex", "credential": "work", "unreachable": true}]),
        json!([{"provider": "codex", "credential": "work", "windows": []}]),
        json!([{"provider": "codex", "credential": "work", "windows": [{"name": "malformed"}]}]),
    ] {
        let app = app(quota, 80.0);
        assert!(rows(&app, 30).is_empty());
        assert!(super::super::rows(&app, 34).is_empty());
        assert!(segment_spans(&app).is_none());
    }
}

#[test]
fn title_and_long_names_fit_the_card_and_its_rows_open_the_view() {
    let app = app(
        json!([{"provider": "a very long provider credential name that will not fit", "credential": "work", "windows": [{"name": "weekly", "percent_used": 35.0}]}]),
        80.0,
    );
    let rows = super::super::rows(&app, 28);
    // The edges open nothing; the title and data rows open the view,
    // and the bar between them opens nothing.
    assert!(rows[0].spot.is_none());
    assert_eq!(rows[1].spot, Some(Spot::Quota));
    assert_eq!(rows[2].spot, Some(Spot::Quota));
    assert!(rows[3].spot.is_none());
    assert!(rows.last().unwrap().spot.is_none());
    assert_eq!(rows[1].line.spans[0].content, "Quota");
    assert!(
        rows[1].line.spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(rows[2].line.spans[0].style, style(Role::Muted));
    assert_eq!(rows[1].line.spans.last().unwrap().style, style(Role::Muted));
    assert!(rows[2].line.to_string().contains('…'));
    assert!(rows[2].line.to_string().ends_with("65%"));
}

#[test]
fn quota_card_snapshots_at_floor_and_sixty_columns() {
    let app = app(
        json!([
            {"provider": "codex", "credential": "work", "windows": [{"name": "weekly", "percent_used": 35.0}]},
            {"provider": "openrouter", "credential": "default", "credit": {"remaining": 1.0, "limit": 10.0}}
        ]),
        80.0,
    );
    for width in [crate::layout::PANEL_FLOOR, 60] {
        let area = Rect::new(0, 0, width, 12);
        let mut buffer = Buffer::empty(area);
        let mut targets = Vec::new();
        super::super::draw(&app, area, &mut buffer, &mut targets);
        // The title and data rows open the quota view; the snapshot pins the text.
        assert!(!targets.is_empty());
        assert!(
            targets
                .iter()
                .all(|target| target.id == crate::mouse::TargetId::Panel(Spot::Quota)),
            "{targets:?}"
        );
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            insta::assert_snapshot!(format!("quota_card_{width}"), crate::view::text(&buffer));
        });
    }
}
