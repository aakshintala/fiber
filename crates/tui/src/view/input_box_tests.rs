//! Tests for the input box's surface: its tint, its edges, its cursor and
//! short screens (`docs/tui.md`, "Look").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::draw;
use crate::app::App;
use crate::keys::{Edit, Key};
use crate::theme::Role;
use contract::clock::Clock;

/// An attached app `width` by `height` with `draft` typed.
fn typed(width: u16, height: u16, draft: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let now = fakes::clock::FakeClock::new().now();
    for ch in draft.chars() {
        app.on_key(Key::Char(ch), now);
    }
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
fn input_box_on_its_surface_80x12() {
    insta::assert_snapshot!(
        "input_box_on_its_surface_80x12",
        screen(&typed(80, 12, "hi"), 80, 12)
    );
}

#[test]
fn input_box_with_completions_above_its_edge() {
    insta::assert_snapshot!(
        "input_box_with_completions_above_its_edge",
        screen(&typed(80, 12, "/h"), 80, 12)
    );
}

#[test]
fn every_cell_of_the_box_rows_is_surface() {
    let app = typed(80, 12, "hi");
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // One draft row in a room of 11: the box is three rows at the bottom.
    let edge = Role::Surface.color();
    for x in 0..80 {
        assert_eq!(buf[(x, 9)].fg, edge, "top edge at {x}");
        assert_eq!(buf[(x, 11)].fg, edge, "bottom edge at {x}");
        assert_eq!(buf[(x, 10)].bg, edge, "text row at {x}");
    }
    assert_eq!(buf[(0, 10)].symbol(), "▌");
    assert_eq!(buf[(0, 10)].fg, Role::Accent.color());
    assert_eq!(buf[(2, 10)].symbol(), "›");
    assert_eq!(buf[(4, 10)].symbol(), "h");
    assert_eq!(buf[(5, 10)].symbol(), "i");
}

#[test]
fn the_prompt_is_info_and_the_cursor_is_a_dim_block() {
    let app = typed(80, 12, "hi");
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // The prompt in `info`, the draft after it, and the cursor as a dim
    // block at the caret (`docs/tui.md`, "The input box").
    for x in [2, 3] {
        assert_eq!(
            buf[(x, 10)].style().fg,
            Some(Role::Info.color()),
            "cell {x}"
        );
    }
    assert_eq!(buf[(2, 10)].symbol(), "›");
    assert_eq!(buf[(6, 10)].symbol(), "█");
    assert_eq!(buf[(6, 10)].style().fg, Some(Role::Muted.color()));
    assert_eq!(buf[(6, 10)].bg, Role::Surface.color());
}

#[test]
fn the_box_fill_covers_empty_and_wrapped_drafts() {
    // Every cell from the ▄ row to the ▀ row is the surface tint edge
    // to edge, past the placeholder or the written text (`docs/tui.md`,
    // "The input box").
    for draft in ["", "hi", &"w".repeat(200)] {
        let app = typed(80, 12, draft);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        crate::view::render(&app, area, &mut buf, None);
        let edge = Role::Surface.color();
        // The box's text rows sit between its edge rows at the bottom.
        let bottom = 11;
        let top = (0..=bottom)
            .rev()
            .find(|y| (0..80).any(|x| buf[(x, *y)].symbol() == "▄"))
            .expect("the top edge");
        assert_eq!(buf[(0, bottom)].symbol(), "▀");
        for y in top + 1..bottom {
            for x in 0..80 {
                assert_eq!(buf[(x, y)].bg, edge, "draft {draft:?} at ({x}, {y})");
            }
        }
        for x in 0..80 {
            assert_eq!(buf[(x, top)].fg, edge, "top edge at {x}");
            assert_eq!(buf[(x, bottom)].fg, edge, "bottom edge at {x}");
        }
        // The cursor marks the caret on the draft's last row.
        let inner = crate::surface::inset(80);
        let shown = app.input().rows(inner);
        let (cursor_row, cursor_col) = app.input().cursor(inner);
        assert_eq!(cursor_row, shown.len() - 1);
        let y = bottom - 1;
        let x = 2 + cursor_col;
        assert_eq!(buf[(x, y)].symbol(), "█", "draft {draft:?}");
    }
}

#[test]
fn the_cursor_hides_with_search_open() {
    let mut app = typed(80, 12, "hi");
    app.on_key(
        crate::keys::Key::CtrlF,
        fakes::clock::FakeClock::new().now(),
    );
    assert!(app.find_bar().is_some());
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // Typing goes to the search box: the draft shows without a cursor
    // (`docs/tui.md`, "The input box").
    for cell in buf.content.iter() {
        if cell.symbol() == "█" {
            assert!(
                cell.modifier.contains(ratatui::style::Modifier::DIM),
                "only the bar's block shows"
            );
        }
    }
}

/// One session envelope for the box's keyboard tests.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> crate::link::Line {
    crate::link::Line::Session(contract::Envelope {
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

/// The box draws no cursor while each state below holds the keyboard:
/// every other gate stays true, so flipping any `&&` in the cursor
/// test to `||` would show it (`docs/tui.md`, "The input box",
/// "Keys").
#[test]
fn the_cursor_hides_where_the_box_loses_the_keyboard() {
    // Below the floor the box never draws through the screen: drawn
    // directly, no cursor shows.
    let mut floored = App::new(PathBuf::from("/w"));
    floored.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    floored.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    floored.set_size(30, 8);
    assert!(floored.chrome().floor_line().is_some());
    let area = Rect::new(0, 0, 30, 8);
    let mut buf = Buffer::empty(area);
    let mut bottom = 8;
    draw(&floored, area, &mut bottom, &mut buf, &mut Vec::new());
    assert!(buf.content.iter().all(|cell| cell.symbol() != "█"));
    // An open approval panel takes the keys: drawn directly, the box
    // keeps its draft but shows no cursor.
    let mut panel = typed(60, 12, "hi");
    panel.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review"}),
    ));
    assert!(panel.panel().is_some());
    assert!(panel.focused().is_none());
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let mut bottom = 12;
    draw(&panel, area, &mut bottom, &mut buf, &mut Vec::new());
    assert!(buf.content.iter().all(|cell| cell.symbol() != "█"));
    // The model picker, the key map screen, the usage view, the
    // repository offer and a focused stop each take the keyboard.
    let mut picker = typed(60, 12, "hi");
    picker.on_key(Key::CtrlL, fakes::clock::FakeClock::new().now());
    assert!(picker.model_picker_open());
    assert!(picker.focused().is_none());
    assert!(!super::cursor_shown(&picker));
    let mut keys = typed(60, 12, "hi");
    keys.open_keys();
    assert!(keys.keys_screen_open());
    assert!(keys.focused().is_none());
    assert!(!super::cursor_shown(&keys));
    let mut usage = App::new(PathBuf::from("/w"));
    usage.set_size(60, 12);
    usage.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    usage.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    ));
    usage.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "usage_recorded",
        serde_json::json!({"generation_id": "g_1", "model": "a/b",
            "tokens": {"input": 12, "cache_read": 3,
                "cache_write": {"5m": 4}, "output": 5},
            "input_bytes": 0, "cost": 0.25}),
    ));
    usage.on_edit(Edit::Paste("/usage".to_owned()));
    usage.on_key(Key::Enter, fakes::clock::FakeClock::new().now());
    assert!(usage.session_view_open());
    assert!(usage.focused().is_none());
    assert!(!super::cursor_shown(&usage));
    let mut offer = typed(60, 12, "hi");
    offer.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "repository_code_offered",
        serde_json::json!({"request_id": "r_1", "items": [
            {"kind": "mcp_server", "name": "a", "hash": "h",
                "required": false, "summary": "MCP server: a"}]}),
    ));
    assert!(offer.offer_open());
    assert!(offer.focused().is_none());
    assert!(!super::cursor_shown(&offer));
    // A focused stop takes the keyboard with everything else open.
    let mut focused = typed(60, 12, "hi");
    focused.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    ));
    focused.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({"text": "reply"}),
    ));
    focused.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "turn_completed",
        serde_json::json!({"outcome": "completed"}),
    ));
    focused.on_key(Key::BackTab, fakes::clock::FakeClock::new().now());
    assert!(focused.focused().is_some());
    assert!(!super::cursor_shown(&focused));
}

#[test]
fn only_the_first_draft_row_takes_the_prompt_style() {
    // Exactly one operand of the prompt test is false in each case:
    // past the first draft row in a scrolled box, and past the first
    // shown row in a box at the top (`docs/tui.md`, "The input box").
    fn typed_lines(width: u16, height: u16, lines: &[&str]) -> App {
        let mut app = App::new(PathBuf::from("/w"));
        app.set_size(width, height);
        app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
        let now = fakes::clock::FakeClock::new().now();
        for (at, line) in lines.iter().enumerate() {
            if at > 0 {
                app.on_edit(Edit::ShiftEnter);
            }
            for ch in line.chars() {
                app.on_key(Key::Char(ch), now);
            }
        }
        app
    }
    // Six rows in a four-row box: the first shown row is the third
    // draft row, past the prompt, and keeps the default foreground.
    let app = typed_lines(60, 12, &["l1", "l2", "l3", "l4", "l5", "l6"]);
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    for x in [2, 3] {
        assert_eq!(buf[(x, 7)].symbol(), " ", "cell {x}");
        assert_eq!(buf[(x, 7)].fg, ratatui::style::Color::Reset, "cell {x}");
    }
    // Two rows at the top: the second shown row is past the prompt.
    let app = typed_lines(60, 12, &["hi", "there"]);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    for x in [2, 3] {
        assert_eq!(buf[(x, 10)].symbol(), " ", "cell {x}");
        assert_eq!(buf[(x, 10)].fg, ratatui::style::Color::Reset, "cell {x}");
    }
}

#[test]
fn edges_at_the_areas_top_row_draw() {
    // Squeezed to one row of room, each edge takes the top row: the
    // row guards keep rows at the area's top.
    let app = typed(60, 12, "hi");
    let area = Rect::new(0, 0, 60, 12);
    for (bottom, edge) in [(1, "▀"), (3, "▄")] {
        let mut buf = Buffer::empty(area);
        let mut bottom = bottom;
        draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
        assert_eq!(buf[(0, 0)].symbol(), edge, "bottom {bottom}");
    }
}

#[test]
fn the_cursor_sits_in_the_same_column_as_without_edges() {
    // No edges fit a body of 2; they draw in a body of 11. The column
    // counts the stripe and gap in both.
    let app = typed(60, 12, "hi");
    let (_, edged) = super::cursor_row(&app, 60, 11);
    let (_, plain) = super::cursor_row(&app, 60, 2);
    assert_eq!((edged, plain), (6, 6));
}

#[test]
fn a_screen_too_short_for_the_edges_keeps_the_input_row() {
    // One draft row: the edges draw only when the row plus 2 fits.
    for (height, edges) in [(1, false), (2, false), (3, true), (5, true)] {
        let app = typed(20, height, "hi");
        let area = Rect::new(0, 0, 20, height);
        let mut buf = Buffer::empty(area);
        let mut bottom = height;
        draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
        let edge = Role::Surface.color();
        let text = (0..height).find(|y| (0..20).any(|x| buf[(x, *y)].symbol() == "h"));
        assert_eq!(text, Some(height.saturating_sub(if edges { 2 } else { 1 })));
        let edge_rows = if edges {
            vec![height.saturating_sub(3), height.saturating_sub(1)]
        } else {
            Vec::new()
        };
        for y in 0..height {
            for x in 0..20 {
                assert_eq!(
                    buf[(x, y)].fg == edge,
                    edge_rows.contains(&y),
                    "height {height}, column {x}, row {y}"
                );
            }
        }
    }
}

#[test]
fn the_cursor_counts_the_bottom_edge_only_where_it_draws() {
    let app = typed(60, 12, "hi");
    // One draft row: no edges fit a body of 2, two fit a body of 5.
    assert_eq!(super::cursor_row(&app, 60, 2), (1, 6));
    assert_eq!(super::cursor_row(&app, 60, 5), (2, 6));
}

#[test]
fn the_box_rows_start_past_the_stripe_and_gap() {
    // The draft wraps past the stripe and gap: `▌`, a space, `› `, then
    // the draft (`docs/tui.md`, "Look", "The input box").
    let app = typed(60, 12, "hi");
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    assert_eq!(buf[(0, 10)].symbol(), "▌");
    assert_eq!(buf[(0, 10)].fg, Role::Accent.color());
    assert_eq!(buf[(0, 10)].bg, Role::Surface.color());
    assert_eq!(buf[(1, 10)].symbol(), " ");
    assert_eq!(buf[(1, 10)].bg, Role::Surface.color());
    assert_eq!(buf[(2, 10)].symbol(), "›");
    let row: String = (0..6).map(|x| buf[(x, 10)].symbol().to_owned()).collect();
    assert_eq!(row, "▌ › hi");
}

#[test]
fn the_box_follows_its_area_x() {
    // Beside a rail the box starts at the area's x: stripe, gap and text
    // shift together, and no cell outside the area changes.
    let app = typed(40, 6, "hi");
    let area = Rect::new(5, 0, 40, 6);
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 6));
    let mut bottom = 6;
    draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
    assert_eq!(buf[(5, 4)].symbol(), "▌");
    assert_eq!(buf[(7, 4)].symbol(), "›");
    for x in 5..45 {
        assert_eq!(buf[(x, 3)].symbol(), "▄", "top edge at {x}");
        assert_eq!(buf[(x, 5)].symbol(), "▀", "bottom edge at {x}");
    }
    for y in 0..6 {
        for x in (0..5).chain(45..50) {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn a_box_three_columns_wide_has_its_stripe_and_two_has_none() {
    // Below three columns there is no stripe and the text keeps the
    // width (`docs/tui.md`, "Look").
    for (width, striped, prompt_x) in [(3, true, 2), (2, false, 0)] {
        let app = typed(width, 8, "h");
        let area = Rect::new(0, 0, width, 8);
        let mut buf = Buffer::empty(area);
        let mut bottom = 8;
        draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
        let stripes = buf
            .content
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .count();
        assert_eq!(stripes > 0, striped, "width {width}");
        let prompt = buf
            .content
            .iter()
            .position(|cell| cell.symbol() == "›")
            .map(|index| index % usize::from(width));
        assert_eq!(prompt, Some(prompt_x), "width {width}");
    }
}

#[test]
fn a_token_target_starts_past_the_stripe() {
    use crate::keys::Edit;
    use crate::mouse::TargetId;

    let mut app = typed(40, 6, "see ");
    let pasted: Vec<String> = (1..=12).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    let area = Rect::new(5, 0, 40, 6);
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 6));
    let mut bottom = 6;
    let mut targets = Vec::new();
    draw(&app, area, &mut bottom, &mut buf, &mut targets);
    // "see " is four columns past the `> ` prompt, itself past the
    // stripe and gap.
    let token = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Token(1)))
        .expect("a token target");
    assert_eq!(token.rect.x, 5 + 2 + 6);
    assert_eq!(token.rect.y, 4);
    assert!(token.rect.right() <= 5 + 40, "{:?}", token.rect);
}

#[test]
fn the_cursor_column_counts_the_stripe_and_gap() {
    let app = typed(60, 12, "hi");
    assert_eq!(super::cursor_row(&app, 60, 11), (2, 6));
    let narrow = typed(2, 12, "hi");
    assert_eq!(super::cursor_row(&narrow, 2, 11), (2, 2));
}

/// The NO_COLOR look: every role paints to the terminal's default.
fn no_color_look() -> crate::look::Look {
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look
}

/// The input box drawn and painted without colour, with its targets and
/// the box's top-edge row.
fn no_color_drawn(app: &App, area: Rect) -> (Buffer, Vec<crate::mouse::Target>, u16) {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let mut targets = Vec::new();
    let top = draw(app, area, &mut bottom, &mut buf, &mut targets);
    no_color_look().paint(&mut buf);
    (buf, targets, top)
}

/// An app `width` by `height` with two queued steering rows.
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

#[test]
fn no_color_draws_the_editing_hint_inside_the_sides() {
    // While a queued message is edited the hint ends at the box's
    // right end, dim, never covering the draft, the caret or the
    // borders (`docs/tui.md`, "Steering").
    let hint = "editing a queued message · Enter amends · ⌥X drops · Esc stops";
    let mut app = queued(80, 12);
    app.select_steering(0);
    // One step left: the caret leaves the hint's first cell free.
    app.on_edit(crate::keys::Edit::Left);
    let area = Rect::new(0, 0, 80, 12);
    let (buf, _, _) = no_color_drawn(&app, area);
    let y = (0..12)
        .find(|y| {
            (0..80)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect::<String>()
                .contains("Esc stops")
        })
        .expect("the hint row");
    // Right-aligned, ending at the last interior column.
    let start = 79 - u16::try_from(crate::format::width(hint)).expect("hint width");
    let mut x = start;
    for ch in hint.chars() {
        let wide = u16::try_from(crate::format::width(&ch.to_string())).expect("char width");
        assert_eq!(buf[(x, y)].symbol(), ch.to_string().as_str(), "cell {x}");
        x = x.saturating_add(wide.max(1));
    }
    assert_eq!(x, 79);
    // The draft, the caret and both sides keep their cells.
    assert_eq!(buf[(1, y)].symbol(), "›");
    assert_eq!(buf[(16, y)].symbol(), "█");
    assert_eq!(buf[(0, y)].symbol(), "│");
    assert_eq!(buf[(79, y)].symbol(), "│");
}

#[test]
fn no_color_clips_the_editing_hint_before_the_side() {
    // Eight columns with the draft cleared: the hint starts at the
    // first free cell and never reaches the box's right side, which
    // keeps its column (`docs/tui.md`, "Steering").
    let mut app = queued(8, 12);
    app.select_steering(0);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..14 {
        app.on_key(Key::Backspace, now);
    }
    assert_eq!(app.input().expand(), "");
    let area = Rect::new(0, 0, 8, 12);
    let (buf, _, _) = no_color_drawn(&app, area);
    let y = (0..12)
        .find(|y| buf[(1, *y)].symbol() == "›")
        .expect("the text row");
    // Past the prompt: the caret keeps its cell, the hint takes the
    // free cells after it, and the side keeps its column.
    let row: String = (0..8).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert_eq!(row, "│› █ing│");
    assert_eq!(buf[(7, y)].symbol(), "│");
    assert_eq!(buf[(0, y)].symbol(), "│");
}

#[test]
fn no_color_boxes_the_input_box_around_the_draft() {
    use ratatui::style::{Color, Modifier};
    let app = typed(40, 6, "hi");
    let area = Rect::new(0, 0, 40, 6);
    let (buf, _, _) = no_color_drawn(&app, area);
    // One draft row: the box is three rows at the bottom, as with
    // colour, but thin.
    for x in 0..40 {
        let top = &buf[(x, 3)];
        assert_eq!(
            top.symbol(),
            if x == 0 {
                "┌"
            } else if x == 39 {
                "┐"
            } else {
                "─"
            }
        );
        assert_eq!((top.fg, top.bg), (Color::Reset, Color::Reset));
        assert_eq!(top.modifier, Modifier::DIM);
        let bottom = &buf[(x, 5)];
        assert_eq!(
            bottom.symbol(),
            if x == 0 {
                "└"
            } else if x == 39 {
                "┘"
            } else {
                "─"
            }
        );
        assert_eq!(bottom.modifier, Modifier::DIM);
    }
    // The text row sits between │ sides: the prompt and draft one
    // column in, the final text character surviving before the side.
    assert_eq!(buf[(0, 4)].symbol(), "│");
    assert_eq!(buf[(39, 4)].symbol(), "│");
    assert_eq!(buf[(1, 4)].symbol(), "›");
    assert_eq!(buf[(2, 4)].symbol(), " ");
    assert_eq!(buf[(3, 4)].symbol(), "h");
    assert_eq!(buf[(4, 4)].symbol(), "i");
    // The drawn cursor sits on the interior's last draft cell, never on
    // a border.
    assert_eq!(buf[(5, 4)].symbol(), "█");
    for y in 3..=5 {
        for x in 0..40 {
            assert_eq!(
                (buf[(x, y)].fg, buf[(x, y)].bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn no_color_wraps_a_full_width_draft_inside_the_sides() {
    // A draft filling the interior but one: the caret takes the last
    // interior cell, and the side keeps its column. The prompt is the
    // renderer's own first two cells.
    let app = typed(10, 6, "12345");
    let area = Rect::new(0, 0, 10, 6);
    let (buf, _, _) = no_color_drawn(&app, area);
    assert_eq!(buf[(0, 4)].symbol(), "│");
    assert_eq!(buf[(9, 4)].symbol(), "│");
    assert_eq!(buf[(1, 4)].symbol(), "›");
    assert_eq!(buf[(7, 4)].symbol(), "5");
    assert_eq!(buf[(8, 4)].symbol(), "█");
    // A draft past the interior wraps at the boxed width: the overflow
    // row draws above, between the same sides.
    let long = typed(10, 6, "123456");
    let (wide, _, _) = no_color_drawn(&long, area);
    let first: String = (0..10).map(|x| wide[(x, 3)].symbol()).collect();
    let second: String = (0..10).map(|x| wide[(x, 4)].symbol()).collect();
    assert_eq!(first, "│› 123456│");
    assert_eq!(second, "│  █     │");
}

#[test]
fn no_color_token_targets_keep_clear_of_the_borders() {
    use crate::keys::Edit;
    use crate::mouse::TargetId;

    let mut app = typed(40, 6, "see ");
    let pasted: Vec<String> = (1..=12).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    let area = Rect::new(5, 0, 40, 6);
    let (buf, targets, _) = no_color_drawn(&app, area);
    let token = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Token(1)))
        .expect("a token target");
    // Past the prompt, one column in from the colour columns: the text
    // rect starts at the box's left side plus one.
    assert_eq!(token.rect.x, 5 + 1 + 6);
    assert!(token.rect.right() < 5 + 40, "{token:?}");
    // The final text character survives beside the right side.
    let y = token.rect.y;
    assert_eq!(buf[(5 + 40 - 1, y)].symbol(), "│");
}

#[test]
fn no_color_cursor_column_counts_one_side() {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let app = typed(60, 12, "hi");
    // The text sits one column in, past the box's left side.
    assert_eq!(super::cursor_row(&app, 60, 11), (2, 5));
    // Too narrow for two sides plus text: the text-first fallback keeps
    // the colour columns.
    let narrow = typed(2, 12, "hi");
    assert_eq!(super::cursor_row(&narrow, 2, 11), (2, 2));
}

#[test]
fn an_open_search_panel_reserves_no_rows() {
    let mut open = typed(80, 12, "hi");
    let now = fakes::clock::FakeClock::new().now();
    open.on_key(Key::CtrlR, now);
    assert!(open.completions().is_some());
    let closed = typed(80, 12, "hi");
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    let mut bottom = area.bottom();
    let top = draw(&open, area, &mut bottom, &mut buf, &mut Vec::new());
    let mut plain = Buffer::empty(area);
    let mut bottomed = area.bottom();
    let topped = draw(&closed, area, &mut bottomed, &mut plain, &mut Vec::new());
    assert_eq!((top, bottom), (topped, bottomed));
}
