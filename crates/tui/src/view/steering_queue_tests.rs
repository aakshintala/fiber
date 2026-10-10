//! Tests for the steering queue's rows: their indent, their heading and
//! footer, their ✕ and narrow widths (`docs/tui.md`, "Steering").

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

/// Renders `app` whole on a `width` by `height` screen as a buffer.
fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
}

#[test]
fn steering_rows_indented_with_heading_and_footer() {
    insta::assert_snapshot!(
        "steering_rows_indented_with_heading_and_footer",
        screen(&queued(60, 12), 60, 12)
    );
}

#[test]
fn the_queue_draws_heading_rows_and_footer() {
    let shown = screen(&queued(80, 12), 80, 12);
    // The heading, each row indented with its ✕ after its text, and the
    // footer: the undroppable row draws no ✕.
    for line in [
        "  • Steering, joins the turn at the next step",
        "  ↳ use the parser  ✕",
        "  ↳ and test it",
        "  ⌥↑ edit · ⌥↓ next · ⌥x drop · click a row to edit, ✕ to drop",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }
}

#[test]
fn the_queue_has_no_stripe() {
    let app = queued(60, 12);
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    // No steering row draws a stripe: the input box keeps its own.
    for target in &targets {
        if !matches!(target.id, TargetId::Steering(_)) {
            continue;
        }
        for y in target.rect.top()..target.rect.bottom() {
            for x in target.rect.left()..target.rect.right() {
                assert_ne!(buf[(x, y)].symbol(), "▌", "a stripe drew");
            }
        }
    }
}

#[test]
fn a_row_shows_its_cross_after_its_text() {
    for width in [30, 80] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        // The droppable row is the oldest: the top steering row, its ✕
        // two spaces past its text.
        let drops: Vec<u16> = targets
            .iter()
            .filter(|target| matches!(target.id, TargetId::DropSteering(_)))
            .map(|target| target.rect.y)
            .collect();
        assert_eq!(drops.len(), 1, "width {width}");
        let row: String = (0..width)
            .map(|x| buf[(x, drops[0])].symbol().to_owned())
            .collect();
        let trimmed = row.trim_end();
        assert!(trimmed.ends_with("✕"), "width {width}: {trimmed:?}");
        assert!(
            trimmed.ends_with("use the parser  ✕"),
            "width {width}: {trimmed:?}"
        );
        // The drop target sits on the ✕'s cell, not the last column.
        let cross = targets
            .iter()
            .find(|target| matches!(target.id, TargetId::DropSteering(_)))
            .expect("a drop target");
        let before = trimmed.strip_suffix('✕').expect("the ✕");
        assert_eq!(
            cross.rect.x,
            u16::try_from(crate::format::width(before)).unwrap_or(u16::MAX),
            "width {width}"
        );
        assert_eq!(cross.rect.width, 1);
        assert_eq!(buf[(width.saturating_sub(1), drops[0])].symbol(), " ");
        for target in &targets {
            if matches!(target.id, TargetId::Steering(_)) {
                assert_eq!(target.rect.width, width, "width {width}");
            }
        }
    }
}

#[test]
fn below_rows_counts_the_queues_heading_and_footer() {
    // One queued message takes three rows below the conversation: its
    // row with the heading above and the footer below; none while empty.
    // The view draws exactly these (`docs/tui.md`, "Steering").
    let mut app = queued(60, 12);
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_queue",
        serde_json::json!({"messages": []}),
    ));
    let bare = app.below_rows();
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_queue",
        serde_json::json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
        ]}),
    ));
    assert_eq!(app.below_rows(), bare + 3);
}

#[test]
fn a_wide_glyph_is_never_split() {
    // `界界` is four columns: cut at the width, never mid-glyph.
    for width in 1..=12u16 {
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
fn narrow_widths_clip_rows_safely() {
    for width in [1, 2, 5] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        // Both rows still draw as targets, clipped without panic.
        assert_eq!(
            targets
                .iter()
                .filter(|target| matches!(target.id, TargetId::Steering(_)))
                .count(),
            2,
            "width {width}"
        );
        // Every drop target sits on a drawn ✕ inside the area.
        for target in &targets {
            if !matches!(target.id, TargetId::DropSteering(_)) {
                continue;
            }
            assert_eq!(target.rect.width, 1, "width {width}");
            assert!(target.rect.right() <= width, "width {width}");
            assert_eq!(
                buf[(target.rect.x, target.rect.y)].symbol(),
                "✕",
                "width {width}"
            );
        }
        // No stripe draws on a steering row at any width: the input
        // box keeps its own.
        for target in &targets {
            if !matches!(target.id, TargetId::Steering(_)) {
                continue;
            }
            for y in target.rect.top()..target.rect.bottom() {
                for x in target.rect.left()..target.rect.right() {
                    assert_ne!(buf[(x, y)].symbol(), "▌", "width {width}");
                }
            }
        }
    }
}

#[test]
fn a_selected_row_marks_attention_with_default_text() {
    let mut app = queued(60, 12);
    app.select_steering(0);
    let buf = buffer(&app, 60, 12);
    // The selected row is the oldest steering row: the indent dim, the
    // mark with its gap in attention, the text in the default foreground,
    // and the ✕ dim.
    let y = buf
        .content
        .chunks(60)
        .position(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("use the parser")
        })
        .and_then(|at| u16::try_from(at).ok())
        .expect("the selected row");
    assert_eq!(buf[(0, y)].style().fg, Role::Muted.color().into());

    for x in [2, 3] {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Attention.color().into(),
            "cell {x}"
        );
    }
    for x in 4..4 + 14 {
        // The text in the default foreground: no colour set.
        assert_eq!(
            buf[(x, y)].style().fg,
            Some(ratatui::style::Color::Reset),
            "cell {x}"
        );
    }
    // The unselected row is dim throughout.
    let plain = buf
        .content
        .chunks(60)
        .position(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("and test it")
        })
        .and_then(|at| u16::try_from(at).ok())
        .expect("the plain row");
    for x in 0..2 + 2 + 11 {
        assert_eq!(
            buf[(x, plain)].style().fg,
            Role::Muted.color().into(),
            "cell {x}"
        );
    }
}

#[test]
fn without_a_selection_the_stripe_stays_accent() {
    let app = queued(60, 12);
    let buf = buffer(&app, 60, 12);
    // The input box's stripe cell: accent, with no editing hint on its
    // rows.
    let stripe = buf
        .content
        .iter()
        .position(|cell| cell.symbol() == "▌")
        .expect("the stripe");
    assert_eq!(buf.content[stripe].style().fg, Role::Accent.color().into());
    let shown = screen(&app, 60, 12);
    assert!(!shown.contains("editing a queued message"), "{shown}");
}

#[test]
fn while_editing_the_stripe_is_attention_with_its_hint() {
    let mut app = queued(80, 12);
    app.select_steering(0);
    // One step left: the caret leaves the hint's first cell free.
    app.on_edit(crate::keys::Edit::Left);
    let buf = buffer(&app, 80, 12);
    let stripe = buf
        .content
        .iter()
        .position(|cell| cell.symbol() == "▌")
        .expect("the stripe");
    assert_eq!(
        buf.content[stripe].style().fg,
        Role::Attention.color().into()
    );
    // The hint ends at the box's right end, dim throughout.
    let hint = "editing a queued message · enter amends · ⌥x drops · esc stops";
    let shown = screen(&app, 80, 12);
    assert!(shown.contains(hint), "{shown}");
    let row = shown
        .lines()
        .find(|row| row.contains("editing a queued message"))
        .expect("the hint row");
    assert!(row.ends_with("esc stops"), "{row:?}");
    let y = shown
        .lines()
        .position(|r| r == row)
        .and_then(|at| u16::try_from(at).ok())
        .expect("the hint row");
    let start = 80 - u16::try_from(crate::format::width(hint)).unwrap_or(u16::MAX);
    for x in start..80 {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Muted.color().into(),
            "cell {x}"
        );
    }
}

#[test]
fn the_hint_never_covers_the_draft_or_the_caret() {
    use crate::keys::Key;
    use contract::clock::Clock;

    let mut app = queued(40, 12);
    app.select_steering(0);
    // A draft reaching the box's right end: the hint clips there instead
    // of covering text.
    let now = fakes::clock::FakeClock::new().now();
    for ch in " and more words to fill the row".chars() {
        app.on_key(Key::Char(ch), now);
    }
    let buf = buffer(&app, 40, 12);
    // The draft's last shown row, the caret's row and column, and where
    // the box draws them, as the input box lays them out.
    let inner = crate::surface::inset(40);
    let shown = app.input().rows(inner);
    let (cursor_row, cursor_col) = app.input().cursor(inner);
    let last = shown.last().expect("a draft row");
    let text_x = 40 - inner;
    // The row holding the draft's last row: its cells match the draft
    // throughout, including the caret's cell.
    let y = (0..12)
        .find(|y| {
            let row: String = (text_x..40)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect();
            row.starts_with(last.as_str())
        })
        .expect("the draft row");
    for (i, ch) in last.chars().enumerate() {
        let x = text_x + u16::try_from(i).unwrap_or(u16::MAX);
        assert_eq!(buf[(x, y)].symbol(), ch.to_string().as_str(), "cell {x}");
    }
    // The caret's cell holds no hint character: the hint skips it, and
    // the draft ends before it. The hint spans the row from the box's
    // left here, so the skip is what keeps this cell blank.
    assert_eq!(cursor_row, shown.len() - 1);
    assert_eq!(buf[(text_x + cursor_col, y)].symbol(), " ");
}
