//! Tests for the steering card's rows: its title, its entries, their
//! ✕, its key hint, and narrow widths (`docs/tui.md`, "Steering").

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

/// An app `width` by `height` with three queued steering rows, each
/// droppable, the last long enough to wrap.
fn three_wrapping(width: u16, height: u16) -> App {
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
            {"content": [{"type": "text", "text": "and test it"}], "source": "driver", "command_id": "c_2"},
            {"content": [{"type": "text", "text": "rewrite the parser loop to take the whole queue at once instead of reading one row per step"}], "source": "driver", "command_id": "c_3"},
        ]}),
    ));
    app
}

/// A renderer that draws whatever the built-in queue draws, so the
/// test observes the built-in cross decision at the callback width.
#[derive(Debug, Default)]
struct BuiltinEcho {
    /// The widths the built-in callback receives, in order.
    widths: std::cell::RefCell<Vec<u16>>,
    /// Optional callback width to keep a returned cross visible at paint width.
    builtin_width: Option<u16>,
}

impl crate::ext::Renderer for BuiltinEcho {
    fn name(&self) -> &str {
        "builtin-test"
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "steering_queue"
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        width: u16,
        builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> crate::ext::Decision {
        let builtin_width = self.builtin_width.unwrap_or(width);
        self.widths.borrow_mut().push(builtin_width);
        crate::ext::Decision::Draw {
            extension: self.name().into(),
            lines: builtin(input, builtin_width),
        }
    }
}

/// The queue row holding `text` on `shown`.
fn queue_row<'a>(shown: &'a str, text: &str) -> &'a str {
    shown
        .lines()
        .find(|row| row.contains(text))
        .unwrap_or_else(|| panic!("no {text:?} row\n{shown}"))
}

#[test]
fn a_replacement_sees_the_builtin_wrap_and_cross_at_its_width() {
    use std::rc::Rc;
    // "use the parser" is 14 cells: with the two-cell mark and the
    // `✕`'s gap the entry's text fits its row exactly at 21 columns,
    // so the built-in draws the row whole with its `✕` at the text's
    // right edge.
    let mut app = queued(21, 12);
    let fake = Rc::new(BuiltinEcho::default());
    app.install_renderer(fake.clone());
    let wide = screen(&app, 21, 12);
    // The built-in title, rows and hint all draw through the
    // replacement (`docs/tui.md`, "What a renderer returns").
    assert!(wide.contains("queued"), "{wide}");
    let fitted = queue_row(&wide, "use the parser");
    assert!(
        fitted.trim_end().ends_with("use the parser ✕"),
        "{fitted:?}"
    );
    let plain = queue_row(&wide, "and test it");
    assert!(!plain.contains('✕'), "{plain:?}");
    // One column short, the built-in wraps the entry, its continuation
    // under the text, and the `✕` stays on the first row. Keep the paint
    // area at 21 so a mutant that wraps at callback width 20 cannot hide
    // behind paint clipping.
    let mut short_app = queued(21, 12);
    let short_fake = Rc::new(BuiltinEcho {
        builtin_width: Some(20),
        ..BuiltinEcho::default()
    });
    short_app.install_renderer(short_fake.clone());
    let short = screen(&short_app, 21, 12);
    assert!(short.contains("queued"), "{short}");
    let wrapped = queue_row(&short, "use the");
    assert!(wrapped.contains("use the"), "{wrapped:?}");
    assert!(wrapped.trim_end().ends_with('✕'), "{wrapped:?}");
    assert!(
        short.lines().any(|row| row.trim_end() == "  parser"),
        "{short}"
    );
    // Exact-fit and one-short built-in results were returned to the same
    // 21-column paint area at callback widths 21 and 20.
    assert_eq!(&*fake.widths.borrow(), &[21]);
    assert_eq!(&*short_fake.widths.borrow(), &[20]);
}

#[test]
fn wide_and_combining_marks_draw_whole() {
    // `界` takes two cells and `é` here is `e` with a combining acute:
    // the rows keep both whole instead of dropping half a glyph or
    // orphaning the mark (`docs/tui.md`, "Steering", "Look").
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
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
            {"content": [{"type": "text", "text": "界界"}], "source": "driver", "command_id": "c_1"},
            {"content": [{"type": "text", "text": "e\u{301}cole"}], "source": "driver", "command_id": "c_2"},
        ]}),
    ));
    let shown = screen(&app, 60, 12);
    // A wide glyph keeps its cells: the second cell of each is the
    // model's blank continuation. The combining mark stays on its base.
    let wide = shown
        .lines()
        .position(|row| row.contains('界'))
        .and_then(|at| u16::try_from(at).ok())
        .expect("the wide row");
    assert!(shown.contains("e\u{301}cole"), "{shown}");
    // The stripe in `attention`, the mark in `attention`, the text in
    // the text colour and the `✕` dim at the text's right edge. Blank
    // continuations of wide glyphs carry no style.
    let buf = buffer(&app, 60, 12);
    assert_eq!(buf[(4, wide)].symbol(), "界");
    assert_eq!(buf[(6, wide)].symbol(), "界");
    let combining = shown
        .lines()
        .position(|row| row.contains("e\u{301}cole"))
        .and_then(|at| u16::try_from(at).ok())
        .expect("the combining row");
    for (y, text_end) in [(wide, 4 + 4), (combining, 4 + 5)] {
        assert_eq!(buf[(0, y)].symbol(), "▌");
        assert_eq!(
            buf[(0, y)].style().fg,
            Role::Attention.color().into(),
            "the stripe"
        );
        for x in [2, 3] {
            assert_eq!(
                buf[(x, y)].style().fg,
                Role::Attention.color().into(),
                "the mark, cell {x}"
            );
        }
        for x in 4..text_end {
            if buf[(x, y)].symbol() == " " {
                continue;
            }
            assert_eq!(
                buf[(x, y)].style().fg,
                Role::Text.color().into(),
                "the text, cell ({x}, {y})"
            );
        }
        assert_eq!(buf[(58, y)].symbol(), "✕");
        assert_eq!(
            buf[(58, y)].style().fg,
            Role::Muted.color().into(),
            "the cross"
        );
    }
}

#[test]
fn steering_card_with_title_entries_and_hint() {
    insta::assert_snapshot!(
        "steering_card_with_title_entries_and_hint",
        screen(&queued(60, 12), 60, 12)
    );
}

#[test]
fn the_card_draws_title_entries_and_hint() {
    let shown = screen(&queued(80, 12), 80, 12);
    // The title with its figure, each entry with its mark, and the key
    // hint: the undroppable row draws no ✕.
    for line in [
        "Steering",
        "2 queued · joins at the next step",
        "↳ use the parser",
        "↳ and test it",
        "⌥↑ edit · ⌥↓ next · ⌥X drop · click a row to edit, ✕ to drop",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }
    let crossed = queue_row(&shown, "use the parser");
    assert!(crossed.trim_end().ends_with('✕'), "{crossed:?}");
    let plain = queue_row(&shown, "and test it");
    assert!(!plain.contains('✕'), "{plain:?}");
}

#[test]
fn three_entries_wrap_with_continuations_under_the_text() {
    let app = three_wrapping(80, 24);
    let shown = screen(&app, 80, 24);
    // The title row, three entries with the long one's continuation
    // aligned under its text, and the key hint.
    for line in [
        "Steering",
        "3 queued · joins at the next step",
        "↳ use the parser",
        "↳ and test it",
        "↳ rewrite the parser loop",
        "one row per step",
        "⌥↑ edit · ⌥↓ next · ⌥X drop · click a row to edit, ✕ to drop",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }
    // The long entry wraps: its continuation starts under its text,
    // past the stripe, the gap and the mark's two columns, with no
    // mark of its own.
    let rows: Vec<&str> = shown.lines().collect();
    let first = rows
        .iter()
        .position(|row| row.contains("rewrite the parser loop"))
        .expect("the long entry's first row");
    let cont = rows.get(first + 1).expect("its continuation");
    assert!(cont.starts_with("▌   "), "{cont:?}");
    assert!(!cont.contains('↳') && !cont.contains('▸'), "{cont:?}");
    insta::assert_snapshot!(
        "three_entries_wrap_with_continuations_under_the_text",
        shown
    );
}

#[test]
fn the_cards_title_marks_text_and_count_keep_their_styles() {
    use ratatui::style::Modifier;
    let app = three_wrapping(80, 24);
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    // The title bold in `attention`.
    let title = text_row(&shown, "Steering");
    let name: String = (2..2 + 8).map(|x| buf[(x, title)].symbol()).collect();
    assert_eq!(name, "Steering", "the title");
    for x in 2..2 + 8 {
        assert_eq!(
            buf[(x, title)].style().fg,
            Role::Attention.color().into(),
            "the title, cell {x}"
        );
        assert!(
            buf[(x, title)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the title is bold, cell {x}"
        );
    }
    // The count dim, right-aligned at the text's right edge.
    let figure = "3 queued · joins at the next step";
    let figure_w = u16::try_from(crate::format::width(figure)).unwrap_or(u16::MAX);
    let start = 79 - figure_w;
    let drawn: String = (start..start + figure_w)
        .map(|x| buf[(x, title)].symbol())
        .collect();
    assert_eq!(drawn, figure, "the count");
    for x in start..start + figure_w {
        assert_eq!(
            buf[(x, title)].style().fg,
            Role::Muted.color().into(),
            "the count, cell {x}"
        );
        assert!(
            !buf[(x, title)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the count is not bold, cell {x}"
        );
    }
    // Every mark in `attention`, every text in the text colour and not
    // dim, every `✕` dim at the text's right edge.
    for needle in ["use the parser", "and test it", "rewrite the parser loop"] {
        let y = text_row(&shown, needle);
        assert_eq!(buf[(2, y)].symbol(), "↳", "the mark");
        for x in [2, 3] {
            assert_eq!(
                buf[(x, y)].style().fg,
                Role::Attention.color().into(),
                "the mark, cell {x}"
            );
        }
        for x in 4..4 + 8 {
            assert_eq!(
                buf[(x, y)].style().fg,
                Role::Text.color().into(),
                "the text, cell ({x}, {y})"
            );
            assert!(
                !buf[(x, y)].style().add_modifier.contains(Modifier::DIM),
                "the text is not dim, cell ({x}, {y})"
            );
        }
        assert_eq!(buf[(78, y)].symbol(), "✕", "the cross");
        assert_eq!(
            buf[(78, y)].style().fg,
            Role::Muted.color().into(),
            "the cross"
        );
    }
    // The long entry's continuation sits under its text in the text
    // colour, with no mark.
    let rows: Vec<&str> = shown.lines().collect();
    let first = rows
        .iter()
        .position(|row| row.contains("rewrite the parser loop"))
        .expect("the long entry's first row");
    let y = u16::try_from(first + 1).expect("its continuation");
    assert_eq!(buf[(4, y)].style().fg, Role::Text.color().into());
    assert!(
        !buf[(4, y)].style().add_modifier.contains(Modifier::DIM),
        "the continuation is not dim"
    );
}

#[test]
fn a_selected_entry_shows_its_mark_and_bold_text() {
    use ratatui::style::Modifier;
    let mut app = three_wrapping(80, 24);
    app.select_steering(2);
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    // The selected entry reads "▸ " in `attention` with its text bold
    // in the text colour.
    let y = text_row(&shown, "rewrite the parser loop");
    assert_eq!(buf[(2, y)].symbol(), "▸", "the mark");
    for x in [2, 3] {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Attention.color().into(),
            "the mark, cell {x}"
        );
    }
    for x in 4..4 + 8 {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Text.color().into(),
            "the text, cell {x}"
        );
        assert!(
            buf[(x, y)].style().add_modifier.contains(Modifier::BOLD),
            "the text is bold, cell {x}"
        );
    }
}

#[test]
fn the_card_has_its_attention_stripe() {
    let app = queued(60, 12);
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let _ = crate::view::render(&app, area, &mut buf, None);
    let shown = screen(&app, 60, 12);
    // Every card text row opens with the stripe in `attention`: the
    // title, each entry and the hint. The input box keeps its own
    // stripe below the card.
    let title = shown
        .lines()
        .position(|row| row.contains("Steering"))
        .expect("the title row");
    let hint = shown
        .lines()
        .position(|row| row.contains("click a row to edit"))
        .expect("the hint row");
    for y in title..=hint {
        let y = u16::try_from(y).expect("a drawn row");
        assert_eq!(buf[(0, y)].symbol(), "▌", "row {y}");
        assert_eq!(
            buf[(0, y)].style().fg,
            Role::Attention.color().into(),
            "row {y}"
        );
    }
}

#[test]
fn a_long_entry_wraps_with_its_cross_right_aligned() {
    // "use the parser" is 14 cells: at 21 columns the entry's text
    // fits its first row exactly, with the `✕` at the text's right
    // edge; at 20 the text wraps under itself and the `✕` stays on the
    // first row's right edge (`docs/tui.md`, "Steering").
    let app = queued(21, 12);
    let area = Rect::new(0, 0, 21, 12);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let cross = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::DropSteering(_)))
        .expect("a drop target");
    // Two columns of padding on the left, one on the right: the `✕`
    // sits on the text's last column, not the card's.
    assert_eq!((cross.rect.x, cross.rect.width), (19, 1));
    let row: String = (0..21)
        .map(|x| buf[(x, cross.rect.y)].symbol().to_owned())
        .collect();
    assert!(row.trim_end().ends_with("↳ use the parser ✕"), "{row:?}");
    let short = queued(20, 12);
    let area = Rect::new(0, 0, 20, 12);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(&short, area, &mut buf, None);
    let cross = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::DropSteering(_)))
        .expect("a drop target");
    assert_eq!((cross.rect.x, cross.rect.width), (18, 1));
    let row: String = (0..20)
        .map(|x| buf[(x, cross.rect.y)].symbol().to_owned())
        .collect();
    assert!(row.trim_end().ends_with('✕'), "{row:?}");
    assert!(row.contains("↳ use the"), "{row:?}");
    // The continuation sits under the text, not under the mark.
    let shown = screen(&short, 20, 12);
    assert!(
        shown.lines().any(|row| row.trim_end() == "▌   parser"),
        "{shown}"
    );
}

#[test]
fn a_droppable_rows_cross_sits_at_the_texts_right_edge() {
    for width in [30, 80] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        // The droppable row is the oldest: the top steering entry, its
        // ✕ on the text's last column.
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
            trimmed.contains("use the parser"),
            "width {width}: {trimmed:?}"
        );
        // The drop target sits on the ✕'s cell: the text's last
        // column, one short of the card's right edge.
        let cross = targets
            .iter()
            .find(|target| matches!(target.id, TargetId::DropSteering(_)))
            .expect("a drop target");
        assert_eq!(cross.rect.x, width - 2, "width {width}");
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
fn below_rows_counts_the_cards_title_entry_hint_and_edges() {
    // One queued message takes five rows below the conversation: the
    // title, its entry and the hint, with an edge row above and below;
    // none while empty. The view draws exactly these
    // (`docs/tui.md`, "Steering").
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
    assert_eq!(app.below_rows(), bare + 5);
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
        // The drawn rows never exceed the width: the card wraps the
        // entry instead of splitting its wide glyph.
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let _ = crate::view::render(&app, area, &mut buf, None);
        for row in crate::view::text(&buf).lines() {
            assert!(
                crate::format::width(row) <= usize::from(width),
                "width {width}: {row:?}"
            );
        }
    }
}

#[test]
fn rows_draw_at_the_areas_top_row() {
    // With one row of room the hint takes it: the rows above it shed,
    // and the kept row stays at the area's top.
    let app = queued(60, 12);
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let mut bottom = 1;
    let mut targets = Vec::new();
    super::draw(&app, area, &mut bottom, &mut buf, &mut targets);
    let row: String = (0..60).map(|x| buf[(x, 0)].symbol().to_owned()).collect();
    assert!(row.contains("click a row to edit"), "{row:?}");
    assert_eq!(bottom, 0);
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
    }
}

/// The screen row holding `needle`.
fn text_row(shown: &str, needle: &str) -> u16 {
    shown
        .lines()
        .position(|row| row.contains(needle))
        .and_then(|at| u16::try_from(at).ok())
        .unwrap_or_else(|| panic!("no {needle:?} row\n{shown}"))
}

#[test]
fn a_selected_row_shows_its_mark_and_bold_text() {
    use ratatui::style::Modifier;
    let mut app = queued(60, 12);
    app.select_steering(0);
    let buf = buffer(&app, 60, 12);
    let shown = screen(&app, 60, 12);
    // The selected row is the oldest steering entry: its mark with its
    // gap in attention, its text bold in the text colour, and its ✕ dim
    // at the text's right edge.
    let y = text_row(&shown, "use the parser");
    for x in [2, 3] {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Attention.color().into(),
            "the mark, cell {x}"
        );
    }
    for x in 4..4 + 14 {
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Text.color().into(),
            "the text, cell {x}"
        );
        assert!(
            buf[(x, y)].style().add_modifier.contains(Modifier::BOLD),
            "the text is bold, cell {x}"
        );
        assert!(
            !buf[(x, y)].style().add_modifier.contains(Modifier::DIM),
            "the text is not dim, cell {x}"
        );
    }
    assert_eq!(buf[(58, y)].symbol(), "✕");
    assert_eq!(
        buf[(58, y)].style().fg,
        Role::Muted.color().into(),
        "the cross"
    );
    // The unselected entry keeps its mark in attention and its text in
    // the text colour, neither dim nor bold.
    let plain = text_row(&shown, "and test it");
    for x in [2, 3] {
        assert_eq!(
            buf[(x, plain)].style().fg,
            Role::Attention.color().into(),
            "the mark, cell {x}"
        );
    }
    for x in 4..4 + 11 {
        assert_eq!(
            buf[(x, plain)].style().fg,
            Role::Text.color().into(),
            "the text, cell {x}"
        );
        assert!(
            !buf[(x, plain)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "the text is not bold, cell {x}"
        );
        assert!(
            !buf[(x, plain)].style().add_modifier.contains(Modifier::DIM),
            "the text is not dim, cell {x}"
        );
    }
}

/// The input box's stripe row: the row holding the prompt.
fn input_stripe_row(shown: &str) -> u16 {
    text_row(shown, "›")
}

#[test]
fn without_a_selection_the_stripe_stays_accent() {
    let app = queued(60, 12);
    let buf = buffer(&app, 60, 12);
    let shown = screen(&app, 60, 12);
    // The input box's stripe cell: accent, with no editing hint on its
    // rows.
    let y = input_stripe_row(&shown);
    assert_eq!(buf[(0, y)].symbol(), "▌");
    assert_eq!(
        buf[(0, y)].style().fg,
        Role::Accent.color().into(),
        "the input box stripe"
    );
    assert!(!shown.contains("editing a queued message"), "{shown}");
}

#[test]
fn while_editing_the_stripe_is_attention_with_its_hint() {
    let mut app = queued(80, 12);
    app.select_steering(0);
    // One step left: the caret leaves the hint's first cell free.
    app.on_edit(crate::keys::Edit::Left);
    let buf = buffer(&app, 80, 12);
    let shown = screen(&app, 80, 12);
    // The input box's stripe cell: attention while a message is edited.
    let y = input_stripe_row(&shown);
    assert_eq!(buf[(0, y)].symbol(), "▌");
    assert_eq!(
        buf[(0, y)].style().fg,
        Role::Attention.color().into(),
        "the input box stripe"
    );
    // The hint ends at the box's right end, dim throughout.
    let hint = "editing a queued message · Enter amends · ⌥X drops · Esc stops";
    assert!(shown.contains(hint), "{shown}");
    let row = shown
        .lines()
        .find(|row| row.contains("editing a queued message"))
        .expect("the hint row");
    assert!(row.ends_with("Esc stops"), "{row:?}");
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
    // The caret's cell holds the drawn cursor, no hint character: the
    // hint skips it, and the draft ends before it. The hint spans the
    // row from the box's left here, so the skip is what keeps this cell
    // out of the hint.
    assert_eq!(cursor_row, shown.len() - 1);
    assert_eq!(buf[(text_x + cursor_col, y)].symbol(), "█");
}

#[test]
fn the_hint_starts_at_the_drafts_end() {
    use crate::keys::Edit;

    // Three steps left: the caret sits three cells back while the
    // draft's end stays put, so the cell exactly at the end takes the
    // hint while the caret keeps the cursor.
    let mut app = queued(60, 12);
    app.select_steering(0);
    for _ in 0..3 {
        app.on_edit(Edit::Left);
    }
    let buf = buffer(&app, 60, 12);
    let y = (0..12)
        .find(|y| {
            (0..60)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect::<String>()
                .contains("Esc sto")
        })
        .expect("the hint row");
    // The caret keeps its covered `s`, drawn by the cursor.
    assert_eq!(buf[(15, y)].symbol(), "s");
    // The cell exactly at the draft's end takes the hint; one past it
    // takes the next character.
    for (x, ch) in [(18, "e"), (19, "s")] {
        assert_eq!(buf[(x, y)].symbol(), ch);
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Muted.color().into(),
            "the cell at the draft's end"
        );
    }
}

#[test]
fn the_hint_clips_at_a_narrow_right_edge() {
    use crate::keys::Key;
    use contract::clock::Clock;

    // Twenty columns with the first row selected and shortened to
    // "use": the hint's cells past the draft draw to the last column,
    // and the cells at and past the edge never draw, without panicking.
    let mut app = queued(20, 12);
    app.select_steering(0);
    assert!(app.steering_selected().is_some());
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..11 {
        app.on_key(Key::Backspace, now);
    }
    assert_eq!(app.input().expand(), "use");
    let buf = buffer(&app, 20, 12);
    let y = (0..12)
        .find(|y| {
            (0..20)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect::<String>()
                .contains("a queued mes")
        })
        .expect("the hint row");
    // The draft's last character and the caret keep their cells: the
    // hint starts past both, at the edge of the text.
    assert_eq!(buf[(6, y)].symbol(), "e");
    assert_eq!(buf[(7, y)].symbol(), "█");
    for (i, ch) in "a queued mes".chars().enumerate() {
        let x = 8 + u16::try_from(i).unwrap_or(u16::MAX);
        assert_eq!(buf[(x, y)].symbol(), ch.to_string().as_str(), "cell {x}");
        assert_eq!(
            buf[(x, y)].style().fg,
            Role::Muted.color().into(),
            "cell {x}"
        );
    }
}

/// A `steering_queue` renderer recording its inputs: the folded lines
/// it returns with the frame each animated call carries.
#[derive(Debug)]
struct RecordingRenderer {
    lines: Vec<Vec<crate::ext::RawSpan>>,
    interval: Option<std::time::Duration>,
    inputs: std::cell::RefCell<Vec<crate::ext::SteeringQueue<'static>>>,
    frames: std::cell::RefCell<Vec<u64>>,
    nows: std::cell::RefCell<Vec<u64>>,
}

impl RecordingRenderer {
    fn lines(texts: &[&str], interval: Option<std::time::Duration>) -> Self {
        Self {
            lines: texts
                .iter()
                .map(|text| {
                    vec![crate::ext::RawSpan {
                        text: (*text).to_owned(),
                        fg: None,
                        bg: None,
                        bold: false,
                        dim: false,
                        italic: false,
                        underline: false,
                        link: None,
                        click: None,
                    }]
                })
                .collect(),
            interval,
            inputs: std::cell::RefCell::new(Vec::new()),
            frames: std::cell::RefCell::new(Vec::new()),
            nows: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn record(&self, input: &crate::ext::Input<'_>) {
        let crate::ext::Input::SteeringQueue(queue) = input else {
            panic!("a steering-queue input, got {input:?}");
        };
        // The recording owns its messages: the fold borrows the app.
        let owned = crate::ext::SteeringQueue {
            messages: queue
                .messages
                .iter()
                .map(|message| crate::ext::SteeringMessage {
                    text: Box::leak(message.text.to_owned().into_boxed_str()) as &str,
                    images: message
                        .images
                        .iter()
                        .map(|image| crate::ext::PromptImage {
                            path: Box::leak(image.path.to_owned().into_boxed_str()) as &str,
                            width: image.width,
                            height: image.height,
                        })
                        .collect(),
                    droppable: message.droppable,
                    selectable: message.selectable,
                    selected: message.selected,
                })
                .collect(),
            flat: queue.flat,
        };
        self.inputs.borrow_mut().push(owned);
    }
}

impl crate::ext::Renderer for RecordingRenderer {
    fn name(&self) -> &str {
        "test"
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "steering_queue"
    }

    fn animate_interval(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
    ) -> Option<std::time::Duration> {
        self.interval
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> crate::ext::Decision {
        self.record(input);
        crate::ext::Decision::Draw {
            extension: "test".into(),
            lines: self.lines.clone(),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the frame and its wall time travel with the draw context"
    )]
    fn render_animated(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        input: &crate::ext::Input<'_>,
        _width: u16,
        frame: u64,
        now: u64,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> (crate::ext::Decision, bool) {
        self.record(input);
        self.frames.borrow_mut().push(frame);
        self.nows.borrow_mut().push(now);
        (
            crate::ext::Decision::Draw {
                extension: "test".into(),
                lines: self.lines.clone(),
            },
            true,
        )
    }
}

/// An app with a three-row queue: a droppable text row, a droppable row
/// with an image, and Fiber's own row.
fn imaged(width: u16, height: u16) -> App {
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
            {"content": [{"type": "text", "text": "first line\nsecond line"}],
             "source": "driver", "command_id": "c_1"},
            {"content": [
                {"type": "text", "text": "look"},
                {"type": "image", "path": "artifacts/a.png",
                 "mime_type": "image/png", "width": 8, "height": 8}],
             "source": "driver", "command_id": "c_2"},
            {"content": [{"type": "text", "text": "note"}],
             "source": "fiber"},
        ]}),
    ));
    app
}

/// Draws `app`'s steering queue alone: the rows' text and the click
/// targets it leaves.
fn draw_queue(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>, u16) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    super::draw(app, area, &mut bottom, &mut buf, &mut targets);
    (crate::view::text(&buf), targets, bottom)
}

#[test]
fn messages_carry_full_text_images_and_flags_oldest_first() {
    let app = imaged(60, 12);
    let messages = app.steering_messages();
    // docs/tui.md:2891: the queued messages oldest-first (three fixture rows).
    assert_eq!(messages.len(), 3); // docs/tui.md:2891: oldest-first messages.
    // The first row keeps its full raw text, newline included: only the
    // laid rows flatten it. docs/tui.md:2891: oldest-first, with text and flags.
    assert_eq!(messages[0].text, "first line\nsecond line"); // docs/tui.md:2891.
    assert!(messages[0].images.is_empty());
    assert!(messages[0].droppable);
    assert!(messages[0].selectable);
    assert!(!messages[0].selected);
    // The second row carries its image's path and size. docs/tui.md:2891: oldest-first.
    assert_eq!(messages[1].text, "look"); // docs/tui.md:2891.
    assert!(messages[1].droppable);
    assert!(!messages[1].selectable);
    let image = messages[1].images.first().expect("one image");
    assert_eq!(image.path, "artifacts/a.png");
    assert_eq!((image.width, image.height), (8, 8));
    // Fiber's own row drops and picks nothing.
    assert!(!messages[2].droppable);
    assert!(!messages[2].selectable);
    assert!(!messages[2].selected);
    // The person's choice lands on the picked row.
    let mut picked = imaged(60, 12);
    picked.select_steering(0);
    let messages = picked.steering_messages();
    assert!(messages[0].selected);
    assert!(!messages[1].selected);
}

#[test]
fn an_empty_replacement_hides_the_whole_card() {
    let mut app = queued(80, 24);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(&[], None));
    app.install_renderer(fake);
    // No rows reserved and none drawn, and no target left behind: the
    // whole card hides. docs/tui.md:2885-2887.
    assert_eq!(super::count(&app, 80), 0); // docs/tui.md:2886: no lines hides the card.
    let (shown, targets, _) = draw_queue(&app, 80, 24);
    assert!(!shown.contains("Steering"), "{shown}");
    assert!(!shown.contains("use the parser"), "{shown}");
    assert!(
        targets.iter().all(|target| !matches!(
            target.id,
            crate::mouse::TargetId::Steering(_) | crate::mouse::TargetId::DropSteering(_)
        )),
        "{targets:?}"
    );
    // The layout drops the hidden rows: without a renderer the same app
    // reserves six rows more, the title, two entries and the hint with
    // an edge row above and below.
    let plain = queued(80, 24);
    // docs/tui.md:2885-2887: the card's six rows hide together.
    assert_eq!(app.below_rows() + 6, plain.below_rows()); // docs/tui.md:2886.
    // With no rows at all and no renderer, the builtin reserves nothing
    // either: the whole card hides with the rows.
    let mut drained = queued(80, 24);
    drained.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_queue",
        serde_json::json!({"messages": []}),
    ));
    assert_eq!(super::count(&drained, 80), 0); // docs/tui.md:2893: empty queue draws nothing.
    let (shown, targets, _) = draw_queue(&drained, 80, 24);
    assert!(!shown.contains("Steering"), "{shown}");
    assert!(
        targets.iter().all(|target| !matches!(
            target.id,
            crate::mouse::TargetId::Steering(_) | crate::mouse::TargetId::DropSteering(_)
        )),
        "{targets:?}"
    );
    // Counting an empty queue runs nothing, even animated with the wall
    // time set: the count stays zero and the renderer sees no call.
    use fakes::clock::FakeClock;
    let clock = FakeClock::new();
    drained.set_now(clock.origin(), 2_000);
    let animated = std::rc::Rc::new(RecordingRenderer::lines(
        &["queued one"],
        Some(std::time::Duration::from_millis(500)),
    ));
    drained.install_renderer(animated.clone());
    assert_eq!(super::count(&drained, 80), 0); // docs/tui.md:2893: empty queue draws nothing.
    assert!(animated.inputs.borrow().is_empty()); // docs/tui.md:2893: no renderer runs.
    assert!(animated.frames.borrow().is_empty()); // docs/tui.md:2893.
}

#[test]
fn a_replacement_draws_without_per_message_targets() {
    let mut app = queued(80, 24);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(
        &["queued one", "queued two"],
        None,
    ));
    app.install_renderer(fake);
    let (shown, targets, _) = draw_queue(&app, 80, 24);
    assert!(shown.contains("queued one"), "{shown}");
    assert!(shown.contains("queued two"), "{shown}");
    assert!(!shown.contains("Steering"), "{shown}");
    // No per-message target survives on a replacement row.
    assert!(
        targets.iter().all(|target| !matches!(
            target.id,
            crate::mouse::TargetId::Steering(_) | crate::mouse::TargetId::DropSteering(_)
        )),
        "{targets:?}"
    );
    // Keys still drop and pick rows under the replacement: dropping the
    // first row sends its `steer_drop` line. The link starts down in
    // these fixtures, so the test raises it the way a hub hello would.
    app.outbox.link = crate::app::Link::Up;
    let crate::app::Effect::Send(lines) = app.drop_steering(0) else {
        panic!("dropping a row sends its line");
    };
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("a command line");
    assert_eq!(line["command"], "steer_drop");
    assert_eq!(line["args"]["command_id"], "c_1");
    app.select_steering(0);
    assert!(app.steering_messages()[0].selected);
}

#[test]
fn an_animated_replacement_records_frame_and_now() {
    use fakes::clock::FakeClock;
    let mut app = queued(80, 24);
    let clock = FakeClock::new();
    app.set_now(clock.origin(), 1_000);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(
        &["queued one"],
        Some(std::time::Duration::from_millis(500)),
    ));
    app.install_renderer(fake.clone());
    let (shown, _, _) = draw_queue(&app, 80, 24);
    assert!(shown.contains("queued one"), "{shown}");
    // docs/tui.md:3121: every animated slot carries frame and now.
    assert_eq!(fake.frames.borrow().clone(), vec![0]); // docs/tui.md:3044: one call on the miss; 3121: frame in input.
    assert_eq!(fake.nows.borrow().clone(), vec![1_000]); // docs/tui.md:3044: one call on the miss; 3121: now in input.
}
