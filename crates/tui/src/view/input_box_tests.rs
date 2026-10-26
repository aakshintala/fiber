//! Tests for the input box's surface: its tint, its edges, its cursor and
//! short screens (`docs/tui.md`, "Look").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::draw;
use crate::app::App;
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::stroke::{Code, Mods, Stroke};
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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

/// An attached app `width` by `height` with one live session, home set
/// so the rail lays out, and `draft` typed.
fn with_card(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        ..Default::default()
    });
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "session_status",
        serde_json::json!({
            "name": "fix the parser", "workspace": "/w", "project": "-w",
            "state": "idle", "started": 0, "calls": 0, "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    app
}

/// Types `draft` into the box.
fn type_draft(app: &mut App, draft: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in draft.chars() {
        app.on_key(Key::Char(ch), now);
    }
}

/// The box prompt's cell: the \u{203a} in the bottom three rows.
fn box_prompt(buf: &Buffer, area: Rect) -> (u16, u16) {
    for y in area.bottom().saturating_sub(3)..area.bottom() {
        for x in area.left()..area.right() {
            if buf[(x, y)].symbol() == "\u{203a}" {
                return (x, y);
            }
        }
    }
    panic!("a box prompt")
}

/// Presses ← through the bindings.
fn press_left(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_press(
        Stroke {
            code: Code::Left,
            mods: Mods::NONE,
        },
        now,
    );
}

#[test]
fn the_box_goes_muted_and_dim_while_the_rail_is_focused() {
    use ratatui::style::Modifier;
    let mut app = with_card(200, 40);
    press_left(&mut app);
    assert!(app.rail_state().focused().is_some());
    // Typing into the box keeps the rail's focus.
    type_draft(&mut app, "hi");
    assert!(app.rail_state().focused().is_some());
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let (x, y) = box_prompt(&buf, area);
    assert_eq!(buf[(x.saturating_sub(2), y)].symbol(), "▌");
    assert_eq!(
        buf[(x.saturating_sub(2), y)].fg,
        Role::Muted.color(),
        "the stripe"
    );
    assert!(buf[(x, y)].modifier.contains(Modifier::DIM), "the prompt");
    assert_eq!(buf[(x.saturating_add(2), y)].symbol(), "h");
    assert!(
        buf[(x.saturating_add(2), y)]
            .modifier
            .contains(Modifier::DIM),
        "the text"
    );
}

#[test]
fn the_box_keeps_its_accent_stripe_without_focus() {
    use ratatui::style::Modifier;
    let mut app = with_card(200, 40);
    type_draft(&mut app, "hi");
    assert_eq!(app.rail_state().focused(), None);
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let (x, y) = box_prompt(&buf, area);
    assert_eq!(buf[(x.saturating_sub(2), y)].symbol(), "▌");
    assert_eq!(
        buf[(x.saturating_sub(2), y)].fg,
        Role::Accent.color(),
        "the stripe"
    );
    assert!(
        !buf[(x.saturating_add(2), y)]
            .modifier
            .contains(Modifier::DIM),
        "the text"
    );
}

#[test]
fn no_color_the_text_stays_dim_while_the_rail_is_focused() {
    use ratatui::style::Modifier;
    let mut app = with_card(200, 40);
    press_left(&mut app);
    assert!(app.rail_state().focused().is_some());
    type_draft(&mut app, "hi");
    assert!(app.rail_state().focused().is_some());
    let area = Rect::new(0, 0, 200, 40);
    let (buf, _, _) = no_color_drawn(&app, area);
    let (x, y) = box_prompt(&buf, area);
    let mut text = 0;
    for at in x..area.right() {
        if buf[(at, y)].symbol() == "h" || buf[(at, y)].symbol() == "i" {
            assert!(
                buf[(at, y)].modifier.contains(Modifier::DIM),
                "the text at {at}"
            );
            text += 1;
        }
    }
    assert_eq!(text, 2, "the draft");
}

/// The attached session every item fixture opens.
const ITEM_SESSION: &str = "s_aaaaaaaaaaaaaaaa";
/// One delegate session.
const ITEM_DELEGATE: &str = "s_dddddddddddddddd";

/// An app on home at 80x24, drawing the default cards.
fn item_home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app
}

/// A live `session_status` for `session`, merged over the defaults.
fn item_live(session: &str, state: serde_json::Value) -> crate::link::Line {
    let mut payload = serde_json::json!({
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
    session_line(session, "session_status", payload)
}

/// Links the app and opens the attached session through home.
fn item_opened(app: &mut App) {
    use contract::SessionId;
    let lines: Vec<serde_json::Value> = app
        .on_line(crate::link::Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines.len(), 2);
    app.on_line(item_live(
        ITEM_SESSION,
        serde_json::json!({"state": "streaming"}),
    ));
    let key = app
        .home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>()
        })
        .and_then(|keys| keys.into_iter().next())
        .unwrap_or_else(|| panic!("a row"));
    let out: Vec<serde_json::Value> = match app
        .on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key)))
    {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    let mut payload = serde_json::Map::new();
    payload.insert(
        "command_id".to_owned(),
        serde_json::Value::String(id.clone()),
    );
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(ITEM_SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.clone(),
    }));
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    let raise: serde_json::Value =
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
    let raise_id = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a raise id"))
        .to_owned();
    payload.insert("command_id".to_owned(), serde_json::Value::String(raise_id));
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(ITEM_SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    }));
}

/// Folds a Fiber delegate as `j_1`.
fn fold_delegate(app: &mut App) {
    app.on_line(session_line(
        ITEM_SESSION,
        "job_started",
        serde_json::json!({"job_id": "j_1", "description": "review the parser",
            "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        ITEM_SESSION,
        "delegate_started",
        serde_json::json!({"job_id": "j_1",
            "delegate_session_id": ITEM_DELEGATE,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Folds a plain job as `j_2`.
fn fold_job(app: &mut App) {
    app.on_line(session_line(
        ITEM_SESSION,
        "job_started",
        serde_json::json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
}

/// Opens `j_1`'s delegate view through the panel.
fn open_delegate(app: &mut App) {
    use crate::mouse::TargetId;
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let _ = app.on_click(TargetId::Panel(crate::app::panel::Spot::Delegate(serial)));
    assert!(app.item_open());
}

/// Opens `j_2`'s job view through the panel.
fn open_job(app: &mut App) {
    use crate::mouse::TargetId;
    let serial = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let _ = app.on_click(TargetId::Panel(crate::app::panel::Spot::Job(serial)));
    assert!(app.item_open());
}

/// The buffer's row holding `needle`.
fn hint_row(buf: &Buffer, needle: &str) -> u16 {
    crate::view::text(buf)
        .lines()
        .enumerate()
        .find_map(|(y, row)| {
            row.contains(needle)
                .then(|| u16::try_from(y).unwrap_or(u16::MAX))
        })
        .unwrap_or_else(|| panic!("a row with {needle}"))
}

/// The hint's cells are dim and never cover the caret.
fn assert_hint(buf: &Buffer, y: u16, hint: &str) {
    let start = 80u16.saturating_sub(u16::try_from(crate::format::width(hint)).unwrap_or(u16::MAX));
    let caret = (0..80)
        .find(|x| buf[(*x, y)].symbol() == "█")
        .unwrap_or_else(|| panic!("the caret"));
    assert!(caret < start, "the hint never covers the caret");
    let mut x = start;
    for ch in hint.chars() {
        let wide = u16::try_from(crate::format::width(&ch.to_string())).unwrap_or(1);
        assert_eq!(buf[(x, y)].symbol(), ch.to_string().as_str(), "cell {x}");
        assert_eq!(buf[(x, y)].fg, Role::Muted.color(), "dim hint {x}");
        x = x.saturating_add(wide.max(1));
    }
}

#[test]
fn delegate_view_hints_that_steering_goes_to_the_delegate() {
    let mut app = item_home();
    item_opened(&mut app);
    fold_delegate(&mut app);
    open_delegate(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let hint = "steering goes to ◆ review the parser";
    let y = hint_row(&buf, hint);
    // The box's last text row carries the hint at its right end.
    assert!(
        crate::view::text(&buf)
            .lines()
            .nth(usize::from(y))
            .unwrap_or_default()
            .trim_end()
            .ends_with(hint),
        "the hint ends the row"
    );
    assert_hint(&buf, y, hint);
}

#[test]
fn job_view_hints_to_type_into_the_job() {
    let mut app = item_home();
    item_opened(&mut app);
    fold_job(&mut app);
    open_job(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let hint = "type into the job";
    let y = hint_row(&buf, hint);
    assert!(
        crate::view::text(&buf)
            .lines()
            .nth(usize::from(y))
            .unwrap_or_default()
            .trim_end()
            .ends_with(hint),
        "the hint ends the row"
    );
    assert_hint(&buf, y, hint);
}

#[test]
fn a_non_empty_draft_draws_no_item_hint() {
    let mut app = item_home();
    item_opened(&mut app);
    fold_delegate(&mut app);
    open_delegate(&mut app);
    type_draft(&mut app, "hi");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    assert!(
        !text.lines().any(|row| row.contains("steering goes to")),
        "no delegate hint with a draft"
    );
    assert!(
        !text.lines().any(|row| row.contains("type into the job")),
        "no job hint with a draft"
    );
}

#[test]
fn an_edit_selection_draws_the_editing_hint_in_an_item_view() {
    let mut app = item_home();
    item_opened(&mut app);
    fold_delegate(&mut app);
    open_delegate(&mut app);
    app.on_line(session_line(
        ITEM_SESSION,
        "steering_queue",
        serde_json::json!({"messages": [
            {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
        ]}),
    ));
    app.select_steering(0);
    assert!(app.steering_selected().is_some());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    assert!(
        text.lines().any(|row| row.contains("queued message")),
        "the editing hint wins"
    );
    assert!(
        !text.lines().any(|row| row.contains("steering goes to")),
        "no item hint while editing"
    );
}

#[test]
fn no_item_view_draws_no_item_hint() {
    let app = typed(80, 24, "");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let text = crate::view::text(&buf);
    assert!(
        !text.lines().any(|row| row.contains("steering goes to")),
        "no delegate hint outside an item view"
    );
    assert!(
        !text.lines().any(|row| row.contains("type into the job")),
        "no job hint outside an item view"
    );
}
