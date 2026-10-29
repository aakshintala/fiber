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
fn a_replacement_sees_the_builtin_cross_only_at_exact_fit() {
    use std::rc::Rc;
    // "use the parser" is 14 cells: with the four-cell prefix and the
    // three-cell cross the row fits exactly at 21 columns.
    let mut app = queued(21, 12);
    let fake = Rc::new(BuiltinEcho::default());
    app.install_renderer(fake.clone());
    let wide = screen(&app, 21, 12);
    // The built-in heading, rows and footer all draw through the
    // replacement (`docs/tui.md`, "What a renderer returns").
    assert!(wide.contains("Steering, joins"), "{wide}");
    let fitted = queue_row(&wide, "use the parser");
    assert!(
        fitted.trim_end().ends_with("use the parser  ✕"),
        "{fitted:?}"
    );
    let plain = queue_row(&wide, "and test it");
    assert!(!plain.contains('✕'), "{plain:?}");
    // One column short, the built-in omits its cross. Keep the paint
    // area at 21 so a mutant that returns the cross at callback width 20
    // cannot hide behind paint clipping.
    let mut short_app = queued(21, 12);
    let short_fake = Rc::new(BuiltinEcho {
        builtin_width: Some(20),
        ..BuiltinEcho::default()
    });
    short_app.install_renderer(short_fake.clone());
    let short = screen(&short_app, 21, 12);
    assert!(short.contains("Steering, joins"), "{short}");
    let omitted = queue_row(&short, "use the parser");
    assert!(!omitted.contains('✕'), "{omitted:?}");
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
    // Dim throughout, over the rows' own spans: the wide row ends
    // after its `✕`, the combining row after its. Blank continuations
    // of wide glyphs carry no style.
    let buf = buffer(&app, 60, 12);
    assert_eq!(buf[(4, wide)].symbol(), "界");
    assert_eq!(buf[(6, wide)].symbol(), "界");
    let combining = shown
        .lines()
        .position(|row| row.contains("e\u{301}cole"))
        .and_then(|at| u16::try_from(at).ok())
        .expect("the combining row");
    for (y, end) in [(wide, 2 + 2 + 4 + 3), (combining, 2 + 2 + 5 + 3)] {
        for x in 0..end {
            if buf[(x, y)].symbol() == " " {
                continue;
            }
            assert_eq!(
                buf[(x, y)].style().fg,
                Role::Muted.color().into(),
                "cell ({x}, {y})"
            );
        }
    }
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
        "  ⌥↑ edit · ⌥↓ next · ⌥X drop · click a row to edit, ✕ to drop",
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
fn a_cross_at_the_exact_fit_draws_and_a_column_less_clips() {
    // "use the parser" is 14 cells: at 21 columns the row with its
    // `✕` fits exactly, and at 20 the text clips one cell short while
    // the `✕` still draws. A wider prefix reserve would drop the `✕`
    // in both (`docs/tui.md`, "Steering").
    for (width, text) in [(21, "use the parser"), (20, "use the parse")] {
        let app = queued(width, 12);
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        let cross = targets
            .iter()
            .find(|target| matches!(target.id, TargetId::DropSteering(_)))
            .expect("a drop target");
        assert_eq!((cross.rect.x, cross.rect.width), (width - 1, 1));
        let row: String = (0..width)
            .map(|x| buf[(x, cross.rect.y)].symbol().to_owned())
            .collect();
        assert!(row.trim_end().ends_with(&format!("{text}  ✕")), "{row:?}");
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
fn rows_draw_at_the_areas_top_row() {
    // With one row of room the footer takes it: the row guard keeps
    // the row at the area's top, and rows above it shed.
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
    let hint = "editing a queued message · Enter amends · ⌥X drops · Esc stops";
    let shown = screen(&app, 80, 12);
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
    assert_eq!(buf[(15, y)].symbol(), "█");
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
fn an_empty_replacement_hides_heading_rows_and_footer() {
    let mut app = queued(80, 24);
    let fake = std::rc::Rc::new(RecordingRenderer::lines(&[], None));
    app.install_renderer(fake);
    // No rows reserved and none drawn, and no target left behind: the
    // heading, the rows and the footer all hide. docs/tui.md:2885-2887.
    assert_eq!(super::count(&app, 80), 0); // docs/tui.md:2886: no lines hides all three.
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
    // reserves four rows more.
    let plain = queued(80, 24);
    // docs/tui.md:2885-2887: heading, rows and footer (four rows) hide together.
    assert_eq!(app.below_rows() + 4, plain.below_rows()); // docs/tui.md:2886.
    // With no rows at all and no renderer, the builtin reserves nothing
    // either: the heading and footer hide with the rows.
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
