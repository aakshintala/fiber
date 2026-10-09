//! Tests for the steering queue's rows: their stripes, their ✕ and narrow
//! widths (`docs/tui.md`, "Steering", "Look").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::link::Line;
use crate::mouse::TargetId;
use crate::theme::Role;

/// One session envelope.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// An app `width` by `height` with two queued steering rows, the first
/// droppable.
fn queued(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "turn_started",
        serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": "hi"}],
        }]}),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_queue",
        serde_json::json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
            {"content": [{"type": "text", "text": "and test it"}], "source": "driver"},
        ]}),
    ));
    app
}

/// Renders `app` whole on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn steering_rows_with_stripes() {
    insta::assert_snapshot!(
        "steering_rows_with_stripes",
        screen(&queued(60, 12), 60, 12)
    );
}

#[test]
fn a_row_keeps_its_cross_on_the_last_column() {
    for width in [5, 80] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        // The droppable row is the oldest: the top steering row.
        let drops: Vec<u16> = targets
            .iter()
            .filter(|target| matches!(target.id, TargetId::DropSteering(_)))
            .map(|target| target.rect.y)
            .collect();
        assert_eq!(drops.len(), 1, "width {width}");
        assert_eq!(buf[(width.saturating_sub(1), drops[0])].symbol(), "✕");
        for target in &targets {
            if matches!(target.id, TargetId::Steering(_)) {
                assert_eq!(target.rect.width, width, "width {width}");
            }
        }
    }
}

#[test]
fn a_wide_glyph_is_never_split() {
    // `界界` is four columns: cut at the inset width, never mid-glyph.
    for width in 1..=5u16 {
        let mut app = App::new(PathBuf::from("/w"));
        app.set_size(width, 12);
        app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "turn_started",
            serde_json::json!({"input": [{
                "type": "message",
                "source": "driver",
                "content": [{"type": "text", "text": "hi"}],
            }]}),
        ));
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "steering_queue",
            serde_json::json!({"messages": [
                {"content": [{"type": "text", "text": "界界"}], "source": "driver"},
            ]}),
        ));
        for row in app.steering() {
            assert!(
                crate::format::width(&row) <= usize::from(width),
                "width {width}: {row:?}"
            );
        }
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        crate::view::render(&app, area, &mut buf, None);
    }
}

#[test]
fn narrow_widths_have_no_stripe() {
    for width in [1, 2] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        for target in &targets {
            if !matches!(target.id, TargetId::Steering(_)) {
                continue;
            }
            for y in target.rect.top()..target.rect.bottom() {
                for x in target.rect.left()..target.rect.right() {
                    assert_ne!(buf[(x, y)].symbol(), "▌", "width {width}");
                    assert_ne!(buf[(x, y)].fg, Role::Accent.color(), "width {width}");
                }
            }
        }
    }
}
