//! Home's frame: snapshots and the cursor.

use super::super::{cursor, render, text};
use super::{TypedRow, typed_body};
use crate::app::{App, Effect, QUIT_HINT};
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

#[test]
fn remote_home_draws_an_unset_dim_chip_and_non_escaping_footer() {
    for typed in [false, true] {
        let mut app = home(80, 24);
        app.set_home(Launch {
            location: crate::LaunchLocation::Remote {
                hub: "box".to_owned(),
            },
            ..Default::default()
        });
        if typed {
            app.on_key(Key::Char('/'), fakes::clock::FakeClock::new().now());
        }
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&app, area, &mut buf, None);
        let drawn = text(&buf);
        assert!(drawn.contains("[no workspace]"), "{drawn}");
        let footer = drawn
            .lines()
            .find(|line| line.contains("↑↓ move · Enter open"))
            .unwrap();
        assert!(!footer.contains("Esc"));
        let y = drawn
            .lines()
            .position(|line| line.contains("[no workspace]"))
            .unwrap();
        let line = drawn.lines().nth(y).unwrap();
        let x = line[..line.find("[no workspace]").unwrap()].chars().count();
        assert!(
            buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())]
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
    }
}

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    home_with_glyph(width, height, "⌇")
}

/// An app on home at `width` by `height` with the one-row logo's `glyph`.
fn home_with_glyph(width: u16, height: u16, glyph: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: glyph.to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(width, height);
    app
}

/// An app on home inside git at `width` by `height`: scoped to the
/// launch project.
fn git_home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: true,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(width, height);
    app
}

/// Renders `app` on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    text(&buf)
}

/// Draws home itself into `area`, below the floor too: home's own guards
/// at widths the floor line covers on screen.
fn home_only(app: &App, area: Rect, buf: &mut Buffer) -> Vec<crate::mouse::Target> {
    let screen = app.home_screen().expect("home draws");
    super::render(app, &screen, area, buf, None)
}

/// Home with `count` live sessions in one project.
fn home_with_live_rows(width: u16, height: u16, count: u8) -> App {
    let mut app = home(width, height);
    app.on_line(hello());
    for n in 0..count {
        app.on_line(status(&format!("s_{n:016x}"), "session", live()));
    }
    app
}

/// The row holding the box's full-width top edge.
fn box_top(frame: &str, width: usize) -> usize {
    let edge = "▄".repeat(width);
    frame
        .lines()
        .position(|row| row == edge)
        .unwrap_or_else(|| panic!("box top missing:\n{frame}"))
}

/// Chooses a model for this session only: with no model on the chip,
/// Enter opens the picker instead of starting, so the blocker tests
/// pick through it, keeping the `[no model]` chips their snapshots show.
fn choose_session_model(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlL, now), crate::app::Effect::None);
    app.on_models(Ok(crate::catalogue::Catalogue {
        installable: Vec::new(),
        models: vec![crate::catalogue::ModelEntry {
            reference: "test/model".to_owned(),
            provider: "test".to_owned(),
            id: "model".to_owned(),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        }],
        notices: Vec::new(),
        lists: Vec::new(),
    }));
    assert_eq!(
        app.on_press(crate::stroke::Stroke::parse("ctrl+s").unwrap(), now),
        crate::app::Effect::None
    );
    assert!(!app.model_picker_open());
}

/// Types `text` into the draft, `\n` as Shift+Enter.
fn type_draft(app: &mut App, text: &str) {
    let now = fakes::clock::FakeClock::new().now();
    for ch in text.chars() {
        if ch == '\n' {
            app.on_edit(crate::keys::Edit::ShiftEnter);
        } else {
            app.on_key(Key::Char(ch), now);
        }
    }
}

#[test]
fn the_cursor_marks_the_last_fitting_row_and_sheds_below_it() {
    // A three-line draft at 80 columns: the caret sits on the last
    // draft row. Drawn below the floor, one row taller it marks the
    // bottom row; one row shorter the row is past the area and nothing
    // draws, without panicking (`docs/tui.md`, "The input box").
    for (height, drawn) in [(6, true), (5, true)] {
        let mut app = home(80, height);
        type_draft(&mut app, "a\nb\nc");
        let area = Rect::new(0, 0, 80, height);
        let mut buf = Buffer::empty(area);
        home_only(&app, area, &mut buf);
        let at: Vec<(u16, u16)> = (0..height)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .filter(|(x, y)| buf[(*x, *y)].symbol() == "█")
            .collect();
        if drawn {
            assert_eq!(at, vec![(5, 3)], "height {height}");
        } else {
            assert!(at.is_empty(), "height {height}: {at:?}");
        }
    }
}

#[test]
fn draft_rows_clip_at_the_areas_bottom_row() {
    // Eight draft rows at 80 by 10: the box shows rows 0 to 5, so the
    // row on the bottom row draws and the rows past it never reach the
    // buffer, without panicking (`docs/tui.md`, "The input box").
    let mut app = home(80, 10);
    type_draft(&mut app, "a\nb\nc\nd\ne\nf\ng\nh");
    let area = Rect::new(0, 0, 80, 10);
    let mut buf = Buffer::empty(area);
    home_only(&app, area, &mut buf);
    let text: String = (0..10)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("  f"), "{text}");
    assert!(text.contains("  h"), "{text}");
}

#[test]
fn home_first_frame_80x24() {
    insta::assert_snapshot!("home_first_frame_80x24", screen(&home(80, 24), 80, 24));
}

#[test]
fn home_first_frame_40x10() {
    insta::assert_snapshot!("home_first_frame_40x10", screen(&home(40, 10), 40, 10));
}

#[test]
fn home_with_a_draft_of_two_lines() {
    let mut app = home(80, 24);
    type_draft(&mut app, "first\nsecond");
    insta::assert_snapshot!("home_with_a_draft_of_two_lines", screen(&app, 80, 24));
}

#[test]
fn home_with_the_slash_panel_above_the_box() {
    let mut app = home(80, 24);
    type_draft(&mut app, "/");
    assert!(app.completions().is_some());
    insta::assert_snapshot!(
        "home_with_the_slash_panel_above_the_box",
        screen(&app, 80, 24)
    );
}

#[test]
fn home_with_a_notice() {
    let mut app = home(80, 24);
    app.connect_failed("Could not reach the hub: refused".to_owned());
    insta::assert_snapshot!("home_with_a_notice", screen(&app, 80, 24));
}

#[test]
fn home_quit_hint() {
    let mut app = home(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlC, now);
    assert!(app.home_screen().is_some_and(|screen| {
        screen
            .foot
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            == QUIT_HINT
    }));
    insta::assert_snapshot!("home_quit_hint", screen(&app, 80, 24));
}

#[test]
fn home_cursor_sits_at_the_draft_cursor_in_the_box() {
    let mut app = home(80, 24);
    type_draft(&mut app, "hi");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    // The bottom-aligned box puts the draft below its top edge; the
    // cursor sits after "› hi" at the inset text column.
    assert_eq!(cursor(&app, area), Some(Position::new(6, 19)));
}

#[test]
fn the_empty_home_prompt_is_info_and_its_text_dim() {
    let app = home(80, 24);
    assert!(app.home_screen().is_some_and(|screen| screen.placeholder));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    // The placeholder's prompt is `info` like a draft's; the rest of
    // the placeholder stays dim (`docs/tui.md`, "The input box"). The
    // drawn cursor covers the placeholder's `/` at the caret.
    let row = (0..24)
        .find(|y| {
            (0..80)
                .map(|x| buf[(x, *y)].symbol().to_owned())
                .collect::<String>()
                .contains("? for shortcuts")
        })
        .expect("the placeholder row");
    for x in [2, 3] {
        assert_eq!(
            buf[(x, row)].style().fg,
            Some(crate::theme::Role::Info.color()),
            "cell {x}"
        );
        assert_eq!(
            buf[(x, row)].bg,
            crate::theme::Role::Surface.color(),
            "cell {x}"
        );
    }
    assert_eq!(buf[(4, row)].symbol(), "█");
    assert!(
        buf[(5, row)]
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::DIM),
        "the placeholder text stays dim"
    );
}

#[test]
fn a_draft_hides_the_placeholder() {
    let mut app = home(80, 24);
    assert!(app.home_screen().is_some_and(|screen| screen.placeholder));
    type_draft(&mut app, "x");
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}

#[test]
fn home_with_live_and_exited_rows() {
    let mut app = home(80, 24);
    let lines: Vec<serde_json::Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let recent = lines[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    app.on_line(accepted(
        recent,
        serde_json::json!({"sessions": [
            recent_row("s_cccccccccccccccc", "old work", "exited"),
            recent_row("s_dddddddddddddddd", "dead work", "crashed"),
        ]}),
    ));
    insta::assert_snapshot!("home_with_live_and_exited_rows", screen(&app, 80, 24));
}

#[test]
fn a_section_with_no_workspace_draws_no_plus_target() {
    let mut app = home(80, 24);
    let outgoing: Vec<serde_json::Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let recent = outgoing[1]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("recent id"));
    let mut row = recent_row("s_aaaaaaaaaaaaaaaa", "orphan", "exited");
    row["workspace"] = serde_json::json!("");
    row["project"] = serde_json::json!("-empty");
    app.on_line(accepted(recent, serde_json::json!({"sessions": [row]})));

    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    assert!(screen(&app, 80, 24).contains("-empty"));
    assert!(
        !targets
            .iter()
            .any(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Start(_))))
    );
}

#[test]
fn a_name_with_control_characters_draws_as_spaces_on_the_frame() {
    // No snapshot: the drawn frame holds the name with spaces, and no
    // raw control character. The printables around each control stay,
    // so flipping the `is_control` predicate fails.
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status(
        "s_aaaaaaaaaaaaaaaa",
        "a\nb\tc\u{1b}d\u{07}e",
        idle(),
    ));
    let drawn = screen(&app, 80, 24);
    // The name's `\n` draws as a space too: the whole name sits on one
    // drawn row, which a raw newline would split.
    assert!(drawn.contains("a b c d e"), "{drawn}");
    for row in drawn.lines() {
        for ch in ['\t', '\u{1b}', '\u{07}'] {
            assert!(!row.contains(ch), "no raw {ch:?}: {drawn}");
        }
    }
}

#[test]
fn home_with_more_rows_than_fit() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "fix the parser", idle()));
    }
    assert_eq!(
        app.home_screen()
            .map(|screen| crate::home::home_test_rows(&screen).len()),
        Some(15)
    );
    insta::assert_snapshot!("home_with_more_rows_than_fit", screen(&app, 80, 24));
}

/// Answers the feed, the first unscoped `recent` page with `rows`, and
/// every scoped ask with `total` and no rows: the held total with one
/// loaded row beneath it.
fn hold_total(app: &mut App, rows: serde_json::Value, total: usize) {
    fn parse(line: &String) -> serde_json::Value {
        serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}"))
    }
    let outgoing: Vec<serde_json::Value> = app.on_line(hello()).iter().map(parse).collect();
    let feed = outgoing
        .iter()
        .find(|line| line["command"] == "feed")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a feed: {outgoing:?}"));
    let recent = outgoing
        .iter()
        .find(|line| line["command"] == "recent" && line.get("args").is_none())
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a recent: {outgoing:?}"));
    assert!(
        app.on_line(accepted(feed, serde_json::json!({})))
            .is_empty()
    );
    let fanout: Vec<serde_json::Value> = app
        .on_line(accepted(recent, rows))
        .iter()
        .map(parse)
        .collect();
    // `docs/tui.md:268` (one scoped ask for the newly seen project).
    assert!(!fanout.is_empty(), "the unscoped answer fans out");
    for line in &fanout {
        let id = line["id"].as_str().unwrap_or_else(|| panic!("a scoped id"));
        assert!(
            app.on_line(accepted(
                id,
                serde_json::json!({"sessions": [], "total": total})
            ))
            .is_empty()
        );
    }
}

#[test]
fn home_header_draws_the_held_total_with_more_beneath() {
    let mut app = home(80, 24);
    hold_total(
        &mut app,
        serde_json::json!({"sessions": [recent_row("s_aaaaaaaaaaaaaaaa", "old work", "exited")]}),
        3,
    );
    let frame = screen(&app, 80, 24);
    assert!(frame.contains("exited · 3"), "{frame}");
    assert!(frame.contains("… 2 more"), "{frame}");
    insta::assert_snapshot!("home_header_with_a_held_total_80x24", frame);
}

#[test]
fn home_header_draws_no_exited_head_at_a_zero_total() {
    let mut app = home(80, 24);
    hold_total(
        &mut app,
        serde_json::json!({"sessions": [recent_row("s_aaaaaaaaaaaaaaaa", "old work", "exited")]}),
        0,
    );
    let frame = screen(&app, 80, 24);
    assert!(!frame.contains("exited ·"), "{frame}");
    assert!(frame.contains("old work"), "{frame}");
}

#[test]
fn home_header_counts_loaded_rows_without_a_total() {
    let mut app = home(80, 24);
    let outgoing: Vec<serde_json::Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let recent = outgoing
        .iter()
        .find(|line| line["command"] == "recent" && line.get("args").is_none())
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a recent: {outgoing:?}"));
    app.on_line(accepted(
        recent,
        serde_json::json!({"sessions": [
            recent_row("s_aaaaaaaaaaaaaaaa", "old work", "exited"),
            recent_row("s_bbbbbbbbbbbbbbbb", "dead work", "crashed"),
        ]}),
    ));
    let frame = screen(&app, 80, 24);
    assert!(frame.contains("exited · 2"), "{frame}");
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// The hub's answer to the `recent` command `id`.
fn accepted(id: &str, result: serde_json::Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            ("result".to_owned(), result),
        ]
        .into_iter()
        .collect(),
    })
}

/// A session spend with `cost` and the common token fields.
fn session_spend(cost: f64) -> serde_json::Value {
    serde_json::json!({
        "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 2},
        "cost": cost,
        "subscription_cost": 0.0,
    })
}

/// A live `session_status` for `session`, named `name`, in `state`.
fn status(session: &str, name: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": "/w",
        "project": "-w",
        "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for `session`, named `name`, in `workspace`.
fn status_in(session: &str, name: &str, workspace: &str) -> Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": workspace,
        "project": "-w",
        "started": 0, "calls": 0, "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in live().as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

#[test]
fn a_start_target_at_the_right_edge_is_not_drawn() {
    let mut app = home(12, 24);
    app.on_line(hello());
    let workspace = format!("/{}", "x".repeat(11));
    app.on_line(status_in("s_aaaaaaaaaaaaaaaa", "row", &workspace));
    let area = Rect::new(0, 0, 12, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    assert!(
        !targets
            .iter()
            .any(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Start(_))))
    );
}

#[test]
fn four_row_header_has_no_count_line_at_the_right_edge() {
    let app = home(35, 24);
    let area = Rect::new(0, 0, 35, 24);
    let mut buf = Buffer::empty(area);
    home_only(&app, area, &mut buf);
    let frame = text(&buf);
    assert!(
        frame
            .lines()
            .take(4)
            .any(|row| { row.contains('▄') || row.contains('▀') || row.contains('█') }),
        "the four-row logo draws: {frame}"
    );
    assert!(
        !frame.contains("no live sessions"),
        "counts have no room: {frame}"
    );
}

#[test]
fn the_picker_keeps_its_selection_drawn() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..9u8 {
        app.on_line(status_in(
            &format!("s_{n:016x}"),
            "fix the parser",
            &format!("/repo{n}"),
        ));
    }
    open_cleared(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    // Ten entries, fewer rows above the box: every selection draws,
    // scrolling the list around it. A picker row shares its row with
    // the frame's pad and gutter, so the entry reads past them.
    for selected in 0..10usize {
        let entry = app
            .home_screen()
            .and_then(|screen| screen.picker)
            .and_then(|picker| {
                (picker.focus == Some(selected))
                    .then(|| picker.recents.get(selected).cloned())
                    .flatten()
            })
            .unwrap_or_else(|| panic!("selection {selected}"));
        let drawn = screen(&app, 80, 24);
        assert!(
            drawn.lines().any(|row| row.contains(&entry)),
            "selection {selected} ({entry}) draws"
        );
        app.on_key(Key::Down, now);
    }
    insta::assert_snapshot!("home_picker_scrolled", screen(&app, 80, 24));
}

/// One exited `recent` row.
fn recent_row(session: &str, name: &str, how: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": how,
    })
}

/// A streaming state.
fn live() -> serde_json::Value {
    serde_json::json!({"state": "streaming"})
}

/// An idle state.
fn idle() -> serde_json::Value {
    serde_json::json!({"state": "idle"})
}

#[test]
fn a_section_with_only_exited_rows_shows_zero_spend() {
    let mut app = home(160, 48);
    let outgoing = app.on_line(hello());
    let recent_id = serde_json::from_str::<serde_json::Value>(&outgoing[1]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.on_line(accepted(
        &recent_id,
        serde_json::json!({"sessions": [recent_row("s_exited", "finished", "exited")]}),
    ));

    let frame = screen(&app, 160, 48);
    assert!(frame.contains("w +  0 live · $0.00"), "{frame}");
    assert!(frame.contains("exited · 1"), "{frame}");
}

#[test]
fn home_header_and_sections_160x48() {
    let mut app = home(160, 48);
    let wall = 172_800_000;
    app.set_wall(wall);
    let outgoing = app.on_line(hello());
    let recent_id = serde_json::from_str::<serde_json::Value>(&outgoing[1]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The 160x48 Look fixture: eight live sessions across three projects,
    // followed by four loaded exited sessions in fiber and one in lens.
    for (id, name, project, workspace, started, since, turns, cost, state) in [
        (
            "s_9f31ac",
            "fix flaky lock test",
            "-fiber",
            "/work/fiber",
            1,
            wall - 16_000,
            4,
            1.35,
            serde_json::json!({"state":"streaming","git":{"branch":"main"},"context":{"tokens":120,"window":1000}}),
        ),
        (
            "s_3ab702",
            "review #688",
            "-fiber",
            "/work/fiber",
            2,
            wall - 1_320_000,
            3,
            1.10,
            serde_json::json!({"state":"waiting","git":{"branch":"review-688"},"context":{"tokens":410,"window":1000},"waiting":{"request_id":"r_1","kind":"approval","summary":"shell cargo publish --dry-run"}}),
        ),
        (
            "s_50c9d2",
            "backfill embeddings",
            "-lens",
            "/work/lens",
            3,
            wall - 720_000,
            9,
            0.87,
            serde_json::json!({"state":"retrying","git":{"branch":"backfill/embed"},"context":{"tokens":330,"window":1000}}),
        ),
        (
            "s_5e91b4",
            "lsp probe",
            "-fiber",
            "/work/fiber",
            4,
            wall - 10_800_000,
            3,
            0.05,
            serde_json::json!({"state":"idle","git":{"branch":"spike/lsp"},"context":{"tokens":910,"window":1000}}),
        ),
        (
            "s_44d2e1",
            "docs: rail spec",
            "-fiber",
            "/work/fiber",
            5,
            wall - 540_000,
            6,
            0.42,
            serde_json::json!({"state":"idle","git":{"branch":"docs/rail-692"},"context":{"tokens":270,"window":1000}}),
        ),
        (
            "s_77aa01",
            "migrate every provider adapter to the new streaming contract",
            "-lens",
            "/work/lens",
            6,
            wall - 120_000,
            12,
            0.64,
            serde_json::json!({"state":"streaming","git":{"branch":"migrate/streaming"},"context":{"tokens":580,"window":1000}}),
        ),
        (
            "s_08f3b9",
            "bump ratatui",
            "-fiber",
            "/work/fiber",
            7,
            wall - 240_000,
            1,
            1.00,
            serde_json::json!({"state":"waiting","git":{"branch":"deps/ratatui-0.30"},"context":{"tokens":640,"window":1000},"waiting":{"request_id":"r_2","kind":"question","summary":"2 of 3 answered"}}),
        ),
        (
            "s_6d1e44",
            "rewrite onboarding tour",
            "-beacon",
            "/work/beacon",
            8,
            wall - 3_600_000,
            7,
            0.35,
            serde_json::json!({"state":"idle","git":{"branch":"tour/rewrite"},"context":{"tokens":190,"window":1000}}),
        ),
    ] {
        let mut state = state;
        state["project"] = serde_json::json!(project);
        state["workspace"] = serde_json::json!(workspace);
        state["started"] = serde_json::json!(started);
        state["since"] = serde_json::json!(since);
        state["turns"] = serde_json::json!(turns);
        state["spend"] = session_spend(cost);
        app.on_line(status(id, name, state));
    }
    app.on_line(left("s_5e91b4", "crashed"));
    let exited = |id: &str, project: &str, workspace: &str, name: &str, since, turns, cost| {
        let mut row = recent_row(id, name, "exited");
        row["project"] = serde_json::json!(project);
        row["workspace"] = serde_json::json!(workspace);
        row["status"] = serde_json::json!({
            "name": name,
            "workspace": workspace,
            "project": project,
            "model": "test/model",
            "state": "idle",
            "since": since,
            "started": 0,
            "calls": 0,
            "turns": turns,
            "spend": session_spend(cost),
            "delegates": 0,
            "jobs": 0,
            "clients": 0,
        });
        row
    };
    app.on_line(accepted(
        &recent_id,
        serde_json::json!({"sessions":[
            exited("s_7f20aa", "-fiber", "/work/fiber", "cut the 0.0.1 release", wall - 10_800_000, 5, 1.10),
            exited("s_61c9d0", "-fiber", "/work/fiber", "speed up the render loop", wall - 18_000_000, 2, 0.58),
            exited("s_88f310", "-fiber", "/work/fiber", "port the router to hyper", wall - 86_400_000, 8, 2.02),
            exited("s_hidden", "-fiber", "/work/fiber", "hidden exited row", 0, 1, 0.01),
            exited("s_50c9a1", "-lens", "/work/lens", "how do I backfill embeddings for old sessions?", 0, 9, 0.87),
        ]}),
    ));
    let frame = screen(&app, 160, 48);
    for text in [
        "2 waiting on you · 2 working · 1 retrying · 1 crashed · 2 ready · $5.78 live",
        "fiber +  5 live · $3.92  2 waiting on you",
        "lens +  2 live · $1.51",
        "beacon +  1 live · $0.35",
        "exited · 4",
        "… 1 more",
        "5 turns",
        "s_50c9a1 · 9 turns · $0.87 · 2d ago",
    ] {
        assert!(frame.contains(text), "{text}: {frame}");
    }
    insta::assert_snapshot!("home_header_and_sections_160x48", frame);
}

#[test]
fn live_row_second_line_joins_id_turns_branch_spend_context_and_the_wait() {
    let mut app = home(160, 48);
    app.set_wall(20_000);
    app.on_line(hello());
    app.on_line(status("s_9f31ac", "review", serde_json::json!({"state":"waiting","since":1000,"turns":4,"git":{"branch":"main"},"context":{"tokens":120,"window":1000},"spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.35,"subscription_cost":0.0},"waiting":{"request_id":"r_1","kind":"approval","summary":"approve publish"}})));
    let frame = screen(&app, 160, 48);
    assert!(
        frame.contains("s_9f31ac · 4 turns · ⎇ main · $1.35 · 12% · approval: approve publish"),
        "{frame}"
    );
}

#[test]
fn a_row_without_turns_draws_no_turns_segment() {
    let mut app = home(160, 48);
    app.set_wall(10_800_000);
    let outgoing = app.on_line(hello());
    let recent_id = serde_json::from_str::<serde_json::Value>(&outgoing[1]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.on_line(status(
        "s_live",
        "live no turns",
        serde_json::json!({"state":"streaming","git":{"branch":"main"}}),
    ));
    let mut old_status = serde_json::json!({"name":"exited no turns","workspace":"/w","project":"-w","model":"test/model","state":"idle","since":0,"started":0,"calls":0,"spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":0.0,"subscription_cost":0.0},"delegates":0,"jobs":0,"clients":0});
    old_status.as_object_mut().unwrap().remove("turns");
    let mut old = recent_row("s_old", "exited no turns", "exited");
    old["status"] = old_status;
    app.on_line(accepted(&recent_id, serde_json::json!({"sessions":[old]})));
    let frame = screen(&app, 160, 48);
    assert!(frame.contains("s_live · ⎇"), "{frame}");
    assert!(frame.contains("s_old · $0.00 · 3h ago"), "{frame}");
    assert!(!frame.contains("s_live · 0 turns"), "{frame}");
    assert!(!frame.contains("s_old · 0 turns"), "{frame}");
}

#[test]
fn one_turn_is_singular_and_zero_draws_0_turns() {
    let mut app = home(160, 48);
    app.on_line(hello());
    app.on_line(status(
        "s_one",
        "one",
        serde_json::json!({"state":"idle","turns":1}),
    ));
    app.on_line(status(
        "s_zero",
        "zero",
        serde_json::json!({"state":"idle","turns":0}),
    ));
    let frame = screen(&app, 160, 48);
    assert!(frame.contains("s_one · 1 turn · $0.00"), "{frame}");
    assert!(frame.contains("s_zero · 0 turns · $0.00"), "{frame}");
}

#[test]
fn an_exited_row_shows_id_turns_spend_and_age() {
    let mut app = home(160, 48);
    app.set_wall(10_800_000);
    let outgoing = app.on_line(hello());
    let recent_id = serde_json::from_str::<serde_json::Value>(&outgoing[1]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut exited = recent_row("s_7f20aa", "release", "exited");
    exited["status"] = serde_json::json!({"name":"release","workspace":"/w","project":"-w","model":"test/model","state":"idle","since":0,"started":0,"calls":0,"turns":5,"spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":1.10,"subscription_cost":0.0},"delegates":0,"jobs":0,"clients":0});
    app.on_line(accepted(
        &recent_id,
        serde_json::json!({"sessions":[exited]}),
    ));
    assert!(screen(&app, 160, 48).contains("s_7f20aa · 5 turns · $1.10 · 3h ago"));
}

#[test]
fn an_exited_row_age_comes_from_the_last_status() {
    let mut app = home(160, 48);
    app.set_wall(10_800_000);
    let outgoing = app.on_line(hello());
    let recent_id = serde_json::from_str::<serde_json::Value>(&outgoing[1]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut exited = recent_row("s_age", "age", "exited");
    exited["status"] = serde_json::json!({"name":"age","workspace":"/w","project":"-w","model":"test/model","state":"idle","since":10_740_000,"started":0,"calls":0,"turns":2,"spend":{"tokens":{"input":1,"cache_read":0,"cache_write":{},"output":2},"cost":0.0,"subscription_cost":0.0},"delegates":0,"jobs":0,"clients":0});
    app.on_line(accepted(
        &recent_id,
        serde_json::json!({"sessions":[exited]}),
    ));
    assert!(screen(&app, 160, 48).contains("s_age · 2 turns · $0.00 · 1m ago"));
}

#[test]
fn a_two_row_line_cuts_the_name_first_at_60_columns() {
    let mut app = home(60, 24);
    app.set_wall(20_000);
    app.on_line(hello());
    app.on_line(status(
        "s_narrow",
        &"long session name ".repeat(8),
        serde_json::json!({"state":"streaming","since":1000,"turns":3}),
    ));
    let frame = screen(&app, 60, 24);
    assert!(frame.contains("WORKING"), "{frame}");
    assert!(frame.contains("19s"), "{frame}");
    insta::assert_snapshot!("home_narrow_rows_60x24", frame);
}

#[test]
fn the_focused_row_is_on_the_selection_bar() {
    let mut app = home(160, 48);
    app.on_line(hello());
    app.on_line(status("s_focused", "focused", live()));
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    app.drawn(&targets);
    // ↑ focuses the only session row from the entry bar.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    let targets = home_only(&app, area, &mut buf);
    let row_target = targets
        .iter()
        .find(|t| matches!(t.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
        .unwrap();
    for y in row_target.rect.y..row_target.rect.y.saturating_add(2) {
        for x in area.x..area.right() {
            assert_eq!(buf[(x, y)].bg, crate::theme::Role::Selection.color());
            assert!(
                !buf[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED),
                "the focused row uses selection, not reverse video"
            );
        }
    }
    let next = row_target.rect.y.saturating_add(2);
    if next < area.bottom() {
        assert_ne!(
            buf[(area.x, next)].bg,
            crate::theme::Role::Selection.color()
        );
    }
}

#[test]
fn the_box_sits_on_the_last_rows_above_the_foot() {
    for (width, height) in [(80, 24), (160, 48)] {
        let frame = screen(&home(width, height), width, height);
        assert!(
            frame
                .lines()
                .nth(usize::from(height - 6))
                .unwrap()
                .starts_with('▄')
        );
        assert!(
            frame
                .lines()
                .nth(usize::from(height - 1))
                .unwrap()
                .contains("Enter opens the focused session")
        );
    }
}

#[test]
fn the_foot_draws_bold_keys_dim_labels_centred_and_cut() {
    use ratatui::style::Modifier;
    let foot = "Enter opens the focused session, or starts one from the box · ↑↓ move · Delete stops a live one, deletes an exited one · F1 the key map · Ctrl+C twice to quit";
    // Wide: the whole foot fits, centred, each key bold and the rest dim.
    let app = home(160, 48);
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let frame = text(&buf);
    let row = frame.lines().nth(47).expect("foot row");
    assert!(row.contains(foot), "{row}");
    let start = row.find("Enter").expect("foot starts");
    assert_eq!(start, (160 - crate::format::width(foot)) / 2);
    let y = 47u16;
    for key in ["Enter", "↑↓", "Delete", "F1", "Ctrl+C"] {
        let at = row.find(key).unwrap_or_else(|| panic!("{key} shows"));
        let x = u16::try_from(row[..at].chars().count()).expect("foot x");
        for dx in 0..key.chars().count() {
            let cell = &buf[(x + u16::try_from(dx).expect("key dx"), y)];
            assert!(cell.modifier.contains(Modifier::BOLD), "{key}");
            assert!(!cell.modifier.contains(Modifier::DIM), "{key}");
        }
    }
    let label_at = start + "Enter".len();
    let label_x = u16::try_from(row[..label_at].chars().count()).expect("label x");
    let label_cell = &buf[(label_x, y)];
    assert!(!label_cell.modifier.contains(Modifier::BOLD));
    assert!(label_cell.modifier.contains(Modifier::DIM));
    // Narrow: the foot is cut at the frame's edge, never wrapped.
    let app = home(40, 10);
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let frame = text(&buf);
    let rows: Vec<&str> = frame.lines().collect();
    let cut: String = foot.chars().take(40).collect();
    assert_eq!(rows[9], cut);
    assert!(!rows[8].contains("twice to quit"));
    assert!(!rows[8].contains("the key map"));
    assert!(buf[(0, 9)].modifier.contains(Modifier::BOLD));
}

#[test]
fn the_box_is_the_full_width_with_the_session_box_inset() {
    let frame = screen(&home(160, 48), 160, 48);
    assert_eq!(frame.lines().nth(42).unwrap(), "▄".repeat(160));
    assert!(frame.lines().nth(43).unwrap().starts_with("▌ › "));
}

#[test]
fn home_empty_160x48() {
    let frame = screen(&home(160, 48), 160, 48);
    assert!(frame.lines().nth(2).unwrap().contains("no live sessions"));
    assert!(frame.contains("No sessions yet."));
    assert!(frame.contains("Type below and press Enter to start one."));
    insta::assert_snapshot!("home_empty_160x48", frame);
}

#[test]
fn the_four_row_header_needs_twelve_rows_and_32_columns() {
    let short = screen(&home(80, 11), 80, 11);
    assert!(short.contains("⌇ fiber 0.0.1"));
    assert!(!short.contains("no live sessions"));
    let tall = screen(&home(80, 12), 80, 12);
    assert!(tall.lines().take(4).any(|line| line.contains('█')));
    assert!(tall.lines().nth(2).unwrap().contains("no live sessions"));
    let narrow_area = Rect::new(0, 0, 31, 24);
    let mut narrow_buf = Buffer::empty(narrow_area);
    home_only(&home(31, 24), narrow_area, &mut narrow_buf);
    let narrow = text(&narrow_buf);
    assert!(narrow.contains("⌇ fiber 0.0.1"), "{narrow}");
}

#[test]
fn at_height_twelve_the_focused_live_rows_first_line_stays_on_screen() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status(
        "s_short",
        "focused first line",
        serde_json::json!({"state":"streaming"}),
    ));
    let initial_area = Rect::new(0, 0, 80, 24);
    let mut initial_buf = Buffer::empty(initial_area);
    let targets = home_only(&app, initial_area, &mut initial_buf);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    app.set_size(80, 12);
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    let frame = text(&buf);
    assert!(
        frame.lines().nth(5).unwrap().contains("focused first line"),
        "{frame}"
    );
    assert!(!frame.lines().nth(6).unwrap().contains("s_short"));
    assert!(
        targets
            .iter()
            .any(|t| t.rect.y == 5 && matches!(t.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
    );
}

#[test]
fn home_four_row_logo_80x24() {
    insta::assert_snapshot!("home_four_row_logo_80x24", screen(&home(80, 24), 80, 24));
}

#[test]
fn home_one_row_logo_at_the_height_limit() {
    let frame = screen(&home(80, 11), 80, 11);
    assert!(frame.contains("⌇ fiber 0.0.1"), "the one-row logo");
    assert!(
        frame.lines().take(4).all(|row| !row.contains('█')),
        "no pixel rows at the height limit"
    );
    insta::assert_snapshot!("home_one_row_logo_at_the_height_limit", frame);
}

#[test]
fn the_logo_glyph_setting_changes_the_one_row_logo() {
    let tall = screen(&home_with_glyph(80, 12, "≈"), 80, 12);
    assert!(tall.contains('█'), "the four-row logo at 12 rows");
    assert!(!tall.contains('≈'), "the four-row logo ignores the setting");
    let short = screen(&home_with_glyph(80, 11, "≈"), 80, 11);
    assert!(short.contains("≈ fiber 0.0.1"), "the one-row glyph setting");
    assert!(!short.contains('⌇'), "no wave where the glyph goes");
}

#[test]
fn a_screen_narrower_than_the_logo_gets_one_row() {
    let area = Rect::new(0, 0, 20, 24);
    let mut buf = Buffer::empty(area);
    home_only(&home(20, 24), area, &mut buf);
    let narrow = text(&buf);
    // The pad of 3 and the one logo row hold no pixel rows; the drawn
    // cursor in the box below is out of the probe.
    assert!(
        narrow.lines().take(4).all(|row| !row.contains('█')),
        "no pixel rows without the width"
    );
    assert!(narrow.contains("⌇ fiber 0.0.1"), "the one-row logo");
}

#[test]
fn home_chips() {
    // The workspace, the model, the thinking level, and what Enter does.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: Some("high".to_owned()),
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    insta::assert_snapshot!("home_chips", screen(&app, 80, 24));
}

/// A remote home before a choice, with the same four workspaces as the
/// prototype's remote picker, expressed as absolute paths on the hub.
fn remote_picker_frame(recents: bool) -> String {
    let mut app = home(80, 24);
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        version: "0.0.1".to_owned(),
        logo_glyph: "⌇".to_owned(),
        model: Some("test/model".to_owned()),
        thinking: Some("high".to_owned()),
        ..Default::default()
    });
    // The picker opens on `~/` and asks for the hub's home listing.
    let sent = directories(Effect::Send(app.on_line(hello())));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, "~/");
    answer(&mut app, &sent[0].0, &["notes", "src", "work"], "/hub");
    if recents {
        for (i, workspace) in [
            "/hub/work/fiber",
            "/hub/work/fiber-worktrees",
            "/hub/work/beacon",
            "/hub/work/pi-rig",
        ]
        .iter()
        .enumerate()
        {
            app.on_line(status_in(&format!("s_{i:016x}"), "hub session", workspace));
        }
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let frame = text(&buf);
    let (y, line) = frame
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("[no workspace]"))
        .unwrap();
    let x = line[..line.find("[no workspace]").unwrap()].chars().count();
    assert!(
        buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())]
            .modifier
            .contains(ratatui::style::Modifier::DIM)
    );
    assert!(frame.contains("Workspaces"));
    let footer = frame
        .lines()
        .find(|line| line.contains("↑↓ move · Enter open"))
        .unwrap();
    assert!(!footer.contains("Esc"));
    assert!(!frame.contains("/sentinel/launch-dir"));
    frame
}

#[test]
fn home_remote_picker_empty() {
    insta::assert_snapshot!("home_remote_picker_empty", remote_picker_frame(false));
}

#[test]
fn home_remote_picker_recent() {
    insta::assert_snapshot!("home_remote_picker_recent", remote_picker_frame(true));
}

#[test]
fn home_picker_open() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    open_listed(&mut app, &["crates", "docs", "research", "scripts"]);
    insta::assert_snapshot!("home_picker_open", screen(&app, 80, 24));
}

#[test]
fn blocker_lines_sit_above_the_box_and_the_list_yields() {
    // A rejected `start` draws its message above the box, wrapped at its
    // width, and takes the same number of rows from the list.
    let mut app = home_with_live_rows(80, 24, 7);
    choose_session_model(&mut app);
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String("start_failed".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String("one\ntwo".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    let frame = screen(&app, 80, 24);
    insta::assert_snapshot!("home_with_blocker_lines", frame.clone());
    let blocked = app.home_screen().expect("home draws");
    let area = Rect::new(0, 0, 80, 24);
    let placed = super::layout(&app, &blocked, area);
    let plain = home_with_live_rows(80, 24, 7);
    let plain_screen = plain.home_screen().expect("home draws");
    let plain_placed = super::layout(&plain, &plain_screen, area);
    assert_eq!(placed.box_top, plain_placed.box_top);
    assert_eq!(
        plain_placed.list_end.saturating_sub(placed.list_end),
        u16::try_from(blocked.blockers.len()).unwrap_or(u16::MAX)
    );
    let blocker_start = placed
        .box_top
        .saturating_sub(u16::try_from(blocked.blockers.len()).unwrap_or(u16::MAX));
    assert_eq!(frame.lines().nth(usize::from(blocker_start)), Some("one"));
    assert_eq!(
        frame
            .lines()
            .nth(usize::from(blocker_start.saturating_add(1))),
        Some("two")
    );
    assert_eq!(usize::from(placed.box_top), box_top(&frame, 80));
    let mut blocked_buf = Buffer::empty(area);
    let blocked_targets = home_only(&app, area, &mut blocked_buf);
    let mut plain_buf = Buffer::empty(area);
    let plain_targets = home_only(&plain, area, &mut plain_buf);
    let entry_targets = |targets: &[crate::mouse::Target]| {
        targets
            .iter()
            .filter(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
            .count()
    };
    assert!(entry_targets(&blocked_targets) < entry_targets(&plain_targets));
}

#[test]
fn blockers_count_against_the_four_row_logo() {
    // Three blocker rows need 15 rows for the four-row logo; one row less
    // uses the one-row logo.
    let mut app = home(80, 14);
    app.on_line(hello());
    choose_session_model(&mut app);
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter sends the start");
    };
    let start: serde_json::Value =
        serde_json::from_str(&lines[0]).unwrap_or_else(|err| panic!("start: {err}"));
    let id = start["id"].as_str().unwrap_or_else(|| panic!("start id"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String("start_failed".to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String("one\ntwo\nthree".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    let text = screen(&app, 80, 14);
    // The pad of 2 and the one logo row hold no pixel rows while the
    // blockers show.
    assert!(
        text.lines().take(3).all(|row| !row.contains('█')),
        "one row while the blockers show"
    );
    assert!(text.contains("⌇ fiber 0.0.1"), "the one-row logo");
    let threshold = screen(&app, 80, 15);
    assert!(threshold.lines().take(4).any(|row| row.contains('█')));
    assert_eq!(
        app.home_screen()
            .map(|screen| screen.blockers)
            .unwrap_or_default(),
        ["one", "two", "three"]
    );
}

#[test]
fn the_banner_yields_a_row_and_the_link_up_frame_is_unchanged() {
    let up = home_with_live_rows(80, 24, 7);
    let up_frame = screen(&up, 80, 24);
    assert_eq!(box_top(&up_frame, 80), 18);

    let mut down = home_with_live_rows(80, 24, 7);
    down.connect_failed("Could not reach the hub: refused".to_owned());
    down.next_retry();
    let down_frame = screen(&down, 80, 24);
    assert!(down_frame.contains("Connection lost · reconnecting"));
    assert_eq!(box_top(&down_frame, 80) + 1, box_top(&up_frame, 80));

    let area = Rect::new(0, 0, 80, 24);
    let mut up_buf = Buffer::empty(area);
    let up_targets = home_only(&up, area, &mut up_buf);
    let mut down_buf = Buffer::empty(area);
    let down_targets = home_only(&down, area, &mut down_buf);
    let entries = |targets: &[crate::mouse::Target]| {
        targets
            .iter()
            .filter(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
            .count()
    };
    assert_eq!(entries(&up_targets), entries(&down_targets) + 1);
}

#[test]
fn home_cursor_hides_while_navigating() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // ↑ focuses the session row, where the draft cursor is hidden.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    assert!(matches!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Entry(_)))
    ));
    assert_eq!(cursor(&app, area), None);
}

#[test]
fn the_placeholder_goes_after_a_start_is_sent() {
    let mut app = home(80, 24);
    app.on_line(crate::link::Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    type_draft(&mut app, "hi");
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Enter, now);
    assert!(app.home_screen().is_some_and(|screen| !screen.placeholder));
}

#[test]
fn a_focused_row_below_the_fold_is_drawn_last() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "fix the parser", idle()));
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let keys: Vec<u64> = app
        .home_screen()
        .map(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(keys.len(), 15);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Entry(
            keys[14]
        )))
    );
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let drawn: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(crate::home::Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert!(drawn.contains(&keys[14]));
    assert!(!drawn.contains(&keys[0]));
    // j steps onto the row's ✕, and the list still ends there.
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app.on_key(Key::Char('j'), now);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Stop(
            keys[14]
        )))
    );
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let drawn: Vec<u64> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(crate::home::Spot::Entry(key)) = target.id {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    assert!(drawn.contains(&keys[14]));
}

#[test]
fn a_focused_live_row_keeps_its_detail_line_at_the_fold() {
    let mut app = home(80, 24);
    app.on_line(hello());
    for n in 0..15u8 {
        let session = format!("s_{n:016x}");
        let title = if n == 14 {
            "focused below fold"
        } else {
            "older row"
        };
        app.on_line(status(&session, title, idle()));
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    app.drawn(&targets);

    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    let frame = screen(&app, 80, 24);
    assert!(frame.contains("focused below fold"), "{frame}");
    assert!(frame.contains("s_000000000000000e"), "{frame}");
}

#[test]
fn home_with_a_focused_row() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    // Two ↑ moves from the empty box to the first of two rows.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    app.on_key(Key::Up, now);
    insta::assert_snapshot!("home_with_a_focused_row", screen(&app, 80, 24));
}

/// A hub `command_rejected` for `id`, with `code` and `message`.
fn refused(id: &str, code: &str, message: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: [
            (
                "command_id".to_owned(),
                serde_json::Value::String(id.to_owned()),
            ),
            (
                "code".to_owned(),
                serde_json::Value::String(code.to_owned()),
            ),
            (
                "message".to_owned(),
                serde_json::Value::String(message.to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// Stopping, accepting, and leaving flow through one screen: three live
/// sessions, two accepted stops, and a leave for the third keep the
/// home list drawn with no fault line.
#[test]
fn home_stop_flow_keeps_the_home_list_drawn() {
    let mut app = home(80, 24);
    let outgoing: Vec<serde_json::Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    let feed = outgoing
        .iter()
        .find(|line| line["command"] == "feed")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a feed: {outgoing:?}"));
    assert!(
        app.on_line(accepted(feed, serde_json::json!({})))
            .is_empty()
    );
    for (session, name) in [
        ("s_aaaaaaaaaaaaaaaa", "first work"),
        ("s_bbbbbbbbbbbbbbbb", "second work"),
        ("s_cccccccccccccccc", "third work"),
    ] {
        app.on_line(status(session, name, live()));
    }
    let now = fakes::clock::FakeClock::new().now();
    let mut stopped = Vec::new();
    focus_first_row(&mut app);
    for _ in 0..2 {
        app.on_edit(crate::keys::Edit::Delete);
        let crate::app::Effect::Send(sent) = app.on_key(Key::Enter, now) else {
            panic!("Enter sends the stop");
        };
        app.on_key(Key::Down, now);
        let lines: Vec<serde_json::Value> = sent
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect();
        let close = lines
            .iter()
            .find(|line| line["command"] == "close")
            .unwrap_or_else(|| panic!("the stop closes: {lines:?}"));
        stopped.push((
            close["session_id"]
                .as_str()
                .unwrap_or_else(|| panic!("a session"))
                .to_owned(),
            close["id"]
                .as_str()
                .unwrap_or_else(|| panic!("a stop id"))
                .to_owned(),
        ));
        assert!(
            app.on_line(accepted(
                &stopped.last().expect("a stop").1,
                serde_json::json!({"session_id": stopped.last().expect("a stop").0})
            ))
            .is_empty()
        );
    }
    assert_ne!(stopped[0].0, stopped[1].0, "two sessions stop");
    // The third session leaves without a stop: one exited row beneath
    // two live ones.
    let third = [
        "s_aaaaaaaaaaaaaaaa",
        "s_bbbbbbbbbbbbbbbb",
        "s_cccccccccccccccc",
    ]
    .into_iter()
    .map(str::to_owned)
    .find(|session| !stopped.iter().any(|(id, _)| id == session))
    .unwrap_or_else(|| panic!("a third session"));
    app.on_line(left(&third, "exited"));
    let frame = screen(&app, 80, 24);
    assert!(frame.contains("exited · 1"), "{frame}");
    // `docs/tui.md:254-255` (one exited row beneath two live ones draws one `○`).
    assert_eq!(frame.matches('○').count(), 1, "{frame}");
    insta::assert_snapshot!("home_stop_flow_keeps_the_home_list_drawn_80x24", frame);
}

/// Focuses the first list entry from the entry bar: Up lands on the
/// last stop, and further Ups hold at the first row.
fn focus_first_row(app: &mut App) {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(app, area, &mut buf);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    let first = app
        .home_screen()
        .and_then(|screen| {
            crate::home::home_test_rows(&screen)
                .first()
                .map(|row| row.0)
        })
        .unwrap_or_else(|| panic!("a first row"));
    let want = Some(crate::mouse::TargetId::Home(crate::home::Spot::Entry(
        first,
    )));
    for _ in 0..10 {
        if app.focused() == want {
            return;
        }
        app.on_key(Key::Up, now);
    }
    panic!("the first row focuses");
}

/// A `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The first row's key.
fn first_key(app: &App) -> u64 {
    app.home_screen()
        .and_then(|screen| {
            crate::home::home_test_rows(&screen)
                .into_iter()
                .next()
                .map(|(key, _)| key)
        })
        .unwrap_or_else(|| panic!("a row"))
}

/// Clicks the first row's ✕.
fn stop_first(app: &mut App) {
    let key = first_key(app);
    app.on_click(crate::mouse::TargetId::Home(Spot::Stop(key)));
}

#[test]
fn home_rows_with_their_x() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    app.on_line(left("s_bbbbbbbbbbbbbbbb", "exited"));
    // An unreadable row ends in no ✕.
    let mut unreadable = status("s_cccccccccccccccc", "future work", live());
    if let Line::Session(status) = &mut unreadable {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(unreadable);
    insta::assert_snapshot!("home_rows_with_their_x", screen(&app, 80, 24));
}

#[test]
fn home_delete_question() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    insta::assert_snapshot!("home_delete_question", screen(&app, 80, 24));
}

#[test]
fn home_cascade_question() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    stop_first(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    let id = delete["id"].as_str().unwrap_or_else(|| panic!("delete id"));
    app.on_line(refused(
        id,
        "session_has_dependents",
        "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too.",
    ));
    insta::assert_snapshot!("home_cascade_question", screen(&app, 80, 24));
}

/// Opens the cascade question naming `dependents`: the plain delete
/// goes out, and the hub refuses naming them all.
fn ask_cascade(app: &mut App, root: &str, dependents: &[String]) {
    stop_first(app);
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    let id = delete["id"].as_str().unwrap_or_else(|| panic!("delete id"));
    let names: Vec<String> = dependents
        .iter()
        .map(|session| format!("`{session}`"))
        .collect();
    app.on_line(refused(
        id,
        "session_has_dependents",
        &format!(
            "Session `{root}` has sessions that continue it: {}. \
            `--cascade` deletes them too.",
            names.join(", ")
        ),
    ));
}

#[test]
fn home_cascade_question_with_many_dependents_wraps_every_id() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=8u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let drawn = screen(&app, 80, 24);
    // The question wraps over as many rows as needed, naming every id
    // the delete adds: none is cut to one line.
    for session in std::iter::once(&"s_0123456789abcdef".to_owned()).chain(&dependents) {
        assert!(drawn.contains(session.as_str()), "{session} is drawn");
    }
    insta::assert_snapshot!("home_cascade_question_wrapped", drawn);
}

#[test]
fn the_delete_question_scrolls_past_the_screen() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let area = Rect::new(0, 0, 80, 14);
    let drawn = app.home_screen().unwrap_or_else(|| panic!("home draws"));
    let max = super::max_question_scroll(&app, &drawn, area);
    assert!(max > 0, "the body runs past the frame");
    // The first window shows the first id, not the last body row.
    let first = screen(&app, 80, 14);
    assert!(first.contains("s_0000000000000001"), "the first id draws");
    assert!(
        !first.contains("It cannot be undone."),
        "the last body row is past the frame"
    );
    // Down to the end shows the last body row and holds there.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..20 {
        app.on_key(Key::Down, now);
    }
    assert_eq!(
        app.home_screen()
            .unwrap_or_else(|| panic!("home draws"))
            .question_scroll,
        max,
        "Down holds at the last window"
    );
    assert!(
        screen(&app, 80, 14).contains("It cannot be undone."),
        "the last body row draws"
    );
    // One Up hides it again, one row higher.
    app.on_key(Key::Up, now);
    assert!(
        !screen(&app, 80, 14).contains("It cannot be undone."),
        "one Up scrolls the last row back out"
    );
}

#[test]
fn one_up_after_down_past_the_end_moves_up_one_row() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    // Down to the last rows, keeping every distinct frame: each Down
    // moves the question by one wrapped row until the last rows draw.
    let now = fakes::clock::FakeClock::new().now();
    let mut frames = vec![screen(&app, 80, 14)];
    loop {
        app.on_key(Key::Down, now);
        let next = screen(&app, 80, 14);
        if next == *frames.last().unwrap_or_else(|| panic!("a first frame")) {
            break;
        }
        frames.push(next);
    }
    assert!(frames.len() > 2, "the question scrolls past the screen");
    // Down past the end keeps the last rows drawn...
    for _ in 0..5 {
        app.on_key(Key::Down, now);
    }
    let end = frames
        .last()
        .unwrap_or_else(|| panic!("a last frame"))
        .clone();
    assert_eq!(
        screen(&app, 80, 14),
        end,
        "Down past the end keeps the last rows"
    );
    // ...so one Up shows the rows one step back.
    app.on_key(Key::Up, now);
    assert_eq!(
        screen(&app, 80, 14),
        frames[frames.len() - 2],
        "one Up scrolls the view up by one row"
    );
}

#[test]
fn home_delete_question_80x14_clipped() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let drawn = screen(&app, 80, 14);
    // The title row keeps its ✕ and the footer its legend while the
    // body clips.
    let title = drawn
        .lines()
        .find(|row| row.contains("Delete session"))
        .unwrap_or_else(|| panic!("a title row"));
    assert!(title.contains("✕"), "the ✕ draws");
    assert!(
        drawn.contains("Enter delete all · Esc keep"),
        "the footer draws"
    );
    assert!(drawn.contains("s_0000000000000001"), "the first id draws");
    assert!(!drawn.contains("It cannot be undone."), "the body clips");
    insta::assert_snapshot!("home_delete_question_80x14_clipped", drawn);
}

#[test]
fn the_delete_frame_keeps_its_title_cross_and_footer_when_clipped() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let now = fakes::clock::FakeClock::new().now();
    // The title row and the footer stay visible in every window: the
    // frame clips the body, never its chrome.
    for _ in 0..=20 {
        let drawn = screen(&app, 80, 14);
        let title = drawn
            .lines()
            .find(|row| row.contains("Delete session"))
            .unwrap_or_else(|| panic!("a title row"));
        assert!(title.contains("✕"), "the ✕ draws");
        assert!(
            drawn.contains("Enter delete all · Esc keep"),
            "the footer draws"
        );
        app.on_key(Key::Down, now);
    }
}

#[test]
fn max_question_scroll_is_body_rows_minus_capacity() {
    // The plain question's body is three rows: the head, a blank row
    // and that deleting is permanent. The frame keeps two edges, two
    // pad rows, the title and its gap and the footer and its gap, so
    // the capacity is the height past 8 rows, floored at one row.
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    let max = |app: &App, width: u16, height: u16| {
        let drawn = app.home_screen().unwrap_or_else(|| panic!("home draws"));
        super::max_question_scroll(app, &drawn, Rect::new(0, 0, width, height))
    };
    assert_eq!(max(&app, 80, 12), 0, "three body rows in four fit");
    assert_eq!(max(&app, 80, 11), 0, "three body rows in three fit");
    assert_eq!(max(&app, 80, 10), 1, "three body rows in two scroll one");
    assert_eq!(max(&app, 80, 9), 2, "three body rows in one scroll two");
    assert_eq!(max(&app, 80, 8), 2, "the capacity floors at one row");
    // The head wraps at the frame's inner width: 43 columns take two
    // rows at 35 to 37 columns, so the body is four rows there.
    assert_eq!(max(&app, 41, 10), 2, "two body rows past two scroll two");
    assert_eq!(max(&app, 40, 10), 2, "two body rows past two scroll two");
    assert_eq!(max(&app, 39, 10), 2, "two body rows past two scroll two");
    // With no question nothing scrolls.
    let plain = home(80, 24);
    let drawn = plain.home_screen().unwrap_or_else(|| panic!("home draws"));
    assert_eq!(
        super::max_question_scroll(&plain, &drawn, Rect::new(0, 0, 80, 10)),
        0,
        "no question scrolls nothing"
    );
}

#[test]
fn a_session_named_with_the_footer_words_keeps_one_legend() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status(
        "s_aaaaaaaaaaaaaaaa",
        "a · Enter delete · Esc keep",
        live(),
    ));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    let drawn = screen(&app, 80, 24);
    // The head keeps the session's name whole, footer words and all.
    let head = drawn
        .lines()
        .find(|row| row.contains("(s_aaaaaaaaaaaaaaaa)"))
        .unwrap_or_else(|| panic!("a head row"));
    assert!(
        head.contains("Delete a · Enter delete · Esc keep"),
        "the name stays whole: {head}"
    );
    // The name's legend and the footer's single legend, nothing else.
    assert_eq!(
        drawn.matches("Enter delete · Esc keep").count(),
        2,
        "the name and the one footer"
    );
    assert!(!drawn.contains("delete all"), "no cascade legend");
}

#[test]
fn the_plain_question_has_no_cascade_text() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    let drawn = screen(&app, 80, 24);
    assert!(drawn.contains("Delete session"), "the frame draws");
    assert!(
        drawn.contains("It cannot be undone."),
        "the last body row draws"
    );
    assert!(
        drawn.contains("Enter delete · Esc keep"),
        "the footer draws"
    );
    assert!(!drawn.contains("--cascade"), "no cascade text");
    assert!(!drawn.contains("delete all"), "no cascade legend");
}

#[test]
fn the_cascade_question_names_the_added_sessions() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    stop_first(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let crate::app::Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    let delete: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    let id = delete["id"].as_str().unwrap_or_else(|| panic!("delete id"));
    app.on_line(refused(
        id,
        "session_has_dependents",
        "Session `s_0123456789abcdef` has sessions that continue it: \
        `s_1111111111111111`, `s_2222222222222222`. `--cascade` deletes them too.",
    ));
    let drawn = screen(&app, 80, 24);
    assert!(
        drawn.contains("s_1111111111111111"),
        "the first added id draws"
    );
    assert!(
        drawn.contains("s_2222222222222222"),
        "the second added id draws"
    );
    assert!(
        drawn.contains("Enter delete all · Esc keep"),
        "the cascade footer draws"
    );
}

#[test]
fn enter_in_the_delete_question_sends_delete_naming_the_session() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    let Effect::Send(lines) = app.on_key(Key::Enter, now) else {
        panic!("Enter deletes");
    };
    assert_eq!(lines.len(), 1);
    let line: serde_json::Value =
        serde_json::from_str(lines.first().unwrap_or_else(|| panic!("a delete line")))
            .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(line["command"], "delete");
    assert_eq!(line["args"]["session"], "s_aaaaaaaaaaaaaaaa");
    assert!(line["args"].get("cascade").is_none(), "no cascade");
}

#[test]
fn esc_in_the_delete_question_keeps_the_session() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    assert!(screen(&app, 80, 24).contains("Delete session"));
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    let drawn = screen(&app, 80, 24);
    assert!(!drawn.contains("Delete session"), "the question closes");
    assert!(drawn.contains("fix the parser"), "the session row remains");
}

#[test]
fn up_and_down_at_the_clip_change_the_drawn_rows() {
    let mut app = home(80, 14);
    app.on_line(hello());
    app.on_line(status("s_0123456789abcdef", "fix the parser", live()));
    app.on_line(left("s_0123456789abcdef", "exited"));
    let dependents: Vec<String> = (1..=20u8).map(|n| format!("s_{n:016x}")).collect();
    ask_cascade(&mut app, "s_0123456789abcdef", &dependents);
    let now = fakes::clock::FakeClock::new().now();
    let top = screen(&app, 80, 14);
    app.on_key(Key::Down, now);
    assert_ne!(screen(&app, 80, 14), top, "Down moves the window");
    for _ in 0..20 {
        app.on_key(Key::Down, now);
    }
    assert!(
        screen(&app, 80, 14).contains("It cannot be undone."),
        "Down holds at the last window"
    );
    for _ in 0..20 {
        app.on_key(Key::Up, now);
    }
    assert_eq!(screen(&app, 80, 14), top, "Up returns to the first window");
}

#[test]
fn other_keys_leave_the_delete_question_open() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    stop_first(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Char('x'), now), Effect::None);
    assert!(
        screen(&app, 80, 24).contains("Delete session"),
        "the question stays open"
    );
}

#[test]
fn focus_does_not_paint_through_the_delete_question() {
    use ratatui::style::Modifier;

    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    // ↑ focuses the only row; Backspace opens its delete question.
    app.on_key(Key::Up, now);
    app.on_key(Key::Backspace, now);
    assert!(
        app.home_screen()
            .unwrap_or_else(|| panic!("home draws"))
            .question
            .is_some(),
        "Backspace opens the question"
    );
    assert!(
        matches!(
            app.focused(),
            Some(crate::mouse::TargetId::Home(Spot::Entry(_)))
        ),
        "focus stays on the row under the box"
    );
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let slab = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Question))
        .map(|target| target.rect)
        .unwrap_or_else(|| panic!("a delete slab target"));
    for y in slab.top()..slab.bottom() {
        for x in slab.left()..slab.right() {
            assert!(
                !buf[(x, y)].modifier.contains(Modifier::REVERSED),
                "no reversed cell inside the delete box at ({x}, {y})"
            );
        }
    }
}
fn ask_quit(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlC, now);
    app.on_key(Key::CtrlC, now);
}

#[test]
fn quit_prompt_on_home() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "tidy docs", idle()));
    ask_quit(&mut app);
    insta::assert_snapshot!("quit_prompt_on_home", screen(&app, 80, 24));
}

#[test]
fn quit_prompt_over_a_session() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.attach(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()));
    ask_quit(&mut app);
    insta::assert_snapshot!("quit_prompt_over_a_session", screen(&app, 80, 24));
}

#[test]
fn a_wide_screen_keeps_the_box_full_width() {
    // The box spans the 120-column area; its text starts two columns in.
    let app = home(120, 24);
    let area = Rect::new(0, 0, 120, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let workspace = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Workspace))
        .unwrap_or_else(|| panic!("a workspace chip target"));
    assert_eq!(workspace.rect.x, 2);
}

#[test]
fn the_chips_join_with_two_spaces_from_the_box_edge() {
    let app = home(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    // The workspace chip opens the picker from the box's first column.
    let workspace = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Workspace))
        .unwrap_or_else(|| panic!("a workspace chip target"));
    assert_eq!(workspace.rect.x, 2);
    let drawn = screen(&app, 80, 24);
    assert!(
        drawn.lines().any(|row| row.contains(
            "[w]  [no model]  [thinking: default]  Enter starts a session"
        )),
        "the chips join with two spaces"
    );
}

#[test]
fn a_chip_starting_on_the_box_edge_takes_no_target_at_any_width() {
    // A chip cut off at the box's right edge keeps its visible cells,
    // and one starting exactly on that edge draws nothing and takes no
    // target. The chips start further right with a long model name, so
    // the sweep runs both: every width from one cell up.
    for model in [None, Some("acme/a-much-longer-model-name-for-the-row")] {
        for width in 1..=120 {
            let mut app = App::new(PathBuf::from("/w"));
            app.set_home(Launch {
                location: crate::LaunchLocation::Local {
                    workspace: PathBuf::from("/w"),
                    project: "-w".to_owned(),
                    git: true,
                },
                hover: true,
                version: "0.0.1".to_owned(),
                model: model.map(str::to_owned),
                thinking: None,
                logo_glyph: "⌇".to_owned(),
                keys: crate::KeysSetup::default(),
                rail_share: 15.0,
                panel_share: 21.0,
                panel_cards: Vec::new(),
                ..Default::default()
            });
            app.set_size(width, 24);
            let area = Rect::new(0, 0, width, 24);
            let mut buf = Buffer::empty(area);
            let targets = render(&app, area, &mut buf, None);
            for target in &targets {
                if let crate::mouse::TargetId::Home(spot) = target.id
                    && spot.is_chip()
                {
                    assert!(
                        target.rect.width > 0,
                        "at width {width}, {spot:?} takes a target with no cells"
                    );
                }
            }
        }
    }
}

#[test]
fn a_row_at_width_one_has_no_entry_target() {
    let mut app = home(1, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    let area = Rect::new(0, 0, 1, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    // No columns for the line: only the ✕ draws, with no entry target.
    assert!(
        targets
            .iter()
            .all(|target| !matches!(target.id, crate::mouse::TargetId::Home(Spot::Entry(_)))),
        "no entry target at width one"
    );
    assert!(
        targets
            .iter()
            .any(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Stop(_)))),
        "the cross still draws"
    );
}

#[test]
fn only_the_selected_picker_row_sits_on_the_bar() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status_in("s_aaaaaaaaaaaaaaaa", "first", "/alpha"));
    app.on_line(status_in("s_bbbbbbbbbbbbbbbb", "second", "/beta"));
    open_cleared(&mut app);
    assert_eq!(
        app.home_screen()
            .and_then(|screen| screen.picker.map(|picker| picker.focus)),
        Some(Some(0))
    );
    let row = |app: &App, entry: &str| {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(app, area, &mut buf, None);
        let drawn = screen(app, 80, 24);
        // The picker draws above the session list, so the first row
        // holding the entry is its picker row.
        let y = drawn
            .lines()
            .position(|line| line.contains(entry))
            .and_then(|row| u16::try_from(row).ok())
            .unwrap_or_else(|| panic!("{entry} draws"));
        let text: String = (0..80).map(|x| buf[(x, y)].symbol().to_owned()).collect();
        // Cell columns, not byte indices: the focused row's gutter
        // holds a three-byte `›` in one cell.
        let at = text.find(entry).unwrap_or_else(|| panic!("{entry} draws"));
        let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
        buf[(x, y)].bg == crate::theme::Role::Accent.color()
    };
    // The launch directory starts selected; moving down bars the next
    // row instead.
    assert!(row(&app, "/w"));
    assert!(!row(&app, "/alpha"));
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Down, now);
    assert!(!row(&app, "/w"));
    assert!(row(&app, "/alpha"));
    assert!(!row(&app, "/beta"));
}

#[test]
fn only_the_selected_completion_sits_on_the_bar() {
    let mut app = home(80, 24);
    type_draft(&mut app, "/");
    let completions = app.completions().unwrap_or_else(|| panic!("completions"));
    assert_eq!(completions.selected, Some(0));
    assert!(completions.lines().len() > 1);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let drawn = screen(&app, 80, 24);
    // The first two matches are home and new: the bar covers the
    // first, the second keeps the surface tint.
    let at = |piece: &str| {
        drawn
            .lines()
            .position(|row| row.contains(piece))
            .and_then(|row| u16::try_from(row).ok())
            .unwrap_or_else(|| panic!("{piece} draws"))
    };
    let accent = crate::theme::Role::Accent.color();
    let surface = crate::theme::Role::Surface.color();
    let cell = |y: u16, piece: &str| {
        let text: String = (0..80).map(|x| buf[(x, y)].symbol().to_owned()).collect();
        let at = text.find(piece).unwrap_or_else(|| panic!("{piece} draws"));
        let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
        buf[(x, y)].bg
    };
    assert_eq!(
        cell(at("Goes home, focused on the session"), "home"),
        accent,
        "{drawn}"
    );
    assert_eq!(
        cell(
            at("Goes home with the cursor in the input box, to pick"),
            "new"
        ),
        surface,
        "{drawn}"
    );
}

#[test]
fn the_hovered_row_is_tinted_and_others_are_not() {
    use ratatui::style::Color;

    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "first", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "second", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let drawn = screen(&app, 80, 24);
    let at = |name: &str| {
        drawn
            .lines()
            .position(|row| row.contains(name))
            .and_then(|row| u16::try_from(row).ok())
            .unwrap_or_else(|| panic!("{name} draws"))
    };
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, Some((0, at("second"))));
    assert_eq!(buf[(0, at("second"))].bg, crate::theme::Role::Hover.color());
    assert_eq!(buf[(0, at("first"))].bg, Color::Reset);
}

#[test]
fn the_paste_token_has_an_exact_target() {
    let mut app = home(80, 24);
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    // "> [Pasted text #1 · 312 lines]" on the draft's first row in
    // the bottom-aligned box.
    let tokens: Vec<ratatui::layout::Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Token(_)))
        .map(|target| target.rect)
        .collect();
    assert_eq!(tokens, vec![Rect::new(2, 19, 28, 1)]);
}

#[test]
fn a_token_on_the_first_hidden_row_has_no_target() {
    let mut app = home(80, 24);
    for n in 0..9 {
        if n > 0 {
            app.on_edit(crate::keys::Edit::ShiftEnter);
        }
        let pasted: Vec<String> = (1..=11).map(|line| format!("line {line}")).collect();
        app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    }
    // Keep the cursor on the first token. The eight-row draft window then
    // ends just before token nine, whose row is still inside the screen.
    for _ in 0..16 {
        app.on_edit(crate::keys::Edit::Left);
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let tokens: Vec<ratatui::layout::Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Token(_)))
        .map(|target| target.rect)
        .collect();
    assert_eq!(tokens.len(), 8);
}

#[test]
fn no_token_target_draws_past_the_visible_rows() {
    // Twenty paste tokens, one per row, with the cursor moved up
    // eleven pieces: the draft scrolls, and the token one row past the
    // visible rows draws no target.
    let mut app = home(80, 24);
    for n in 0..20 {
        if n > 0 {
            app.on_edit(crate::keys::Edit::ShiftEnter);
        }
        let pasted: Vec<String> = (1..=11).map(|n| format!("line {n}")).collect();
        app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    }
    for _ in 0..21 {
        app.on_edit(crate::keys::Edit::Left);
    }
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    // Eight visible token rows, each with its target; the ninth token
    // row, one past them on the chip row, has none.
    let tokens: Vec<ratatui::layout::Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Token(_)))
        .map(|target| target.rect)
        .collect();
    assert_eq!(tokens.len(), 8);
}

#[test]
fn a_token_span_starting_at_the_box_edge_has_no_target() {
    // At width two the prompt fills the box, so every span of the
    // token label starts where the box ends: `end` is `span.start`
    // clipped to the box width, and the `start < end` guard keeps each
    // zero-width span from becoming a target.
    let mut app = home(2, 24);
    let pasted: Vec<String> = (1..=11).map(|n| format!("line {n}")).collect();
    app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    let area = Rect::new(0, 0, 2, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    let tokens: Vec<ratatui::layout::Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Token(_)))
        .map(|target| target.rect)
        .collect();
    assert!(
        tokens.iter().all(|rect| rect.width > 0),
        "no zero-width token target: {tokens:?}"
    );
    assert_eq!(tokens, vec![]);
}

#[test]
fn home_chips_inside_git() {
    // The launch directory is in git, so the new worktree switch sits
    // beside the workspace chip, off.
    insta::assert_snapshot!("home_chips_inside_git", screen(&git_home(80, 24), 80, 24));
}

#[test]
fn home_chips_outside_git() {
    // Outside git the switch is not shown: no placeholder takes its
    // place.
    insta::assert_snapshot!("home_chips_outside_git", screen(&home(80, 24), 80, 24));
}

/// Renders `app` on a `width` by `height` screen, returning its buffer.
fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    render(app, area, &mut buf, None);
    buf
}

/// The row's text, trailing spaces kept.
fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

/// An app on home at `width` by `height` with the workspace picker open
/// browsing the launch directory over two recent workspaces.
fn picking(width: u16, height: u16) -> App {
    let mut app = home(width, height);
    app.on_line(hello());
    app.on_line(status_in("s_0000000000000000", "fix the parser", "/repo0"));
    open_listed(&mut app, &["crates", "docs", "research", "scripts"]);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_some())
    );
    app
}

/// An app on home at `width` by `height` with the workspace picker
/// showing its two recent workspaces alone.
fn picking_recents(width: u16, height: u16) -> App {
    let mut app = home(width, height);
    app.on_line(hello());
    app.on_line(status_in("s_0000000000000000", "fix the parser", "/repo0"));
    open_cleared(&mut app);
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_some())
    );
    app
}

#[test]
fn home_picker_overlay_160x48() {
    insta::assert_snapshot!(
        "home_picker_overlay_160x48",
        screen(&picking(160, 48), 160, 48)
    );
}

/// The workspace picker overlay: the title, the first row barred, the
/// legend footer, centred across and down home.
#[test]
fn the_picker_draws_centred_with_a_bar_and_a_legend() {
    let app = picking_recents(80, 24);
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    // Cell columns, not byte indices: home's rows may hold a
    // three-byte `›` in one cell before the slab.
    let col = |text: &str, piece: &str| {
        let at = text.find(piece).unwrap_or_else(|| panic!("{piece} draws"));
        u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX)
    };
    // The title reads bold in `accent`.
    let title = u16::try_from(
        shown
            .lines()
            .position(|row| row.contains("Workspaces"))
            .expect("the title"),
    )
    .unwrap_or(u16::MAX);
    let text = row_text(&buf, title, 80);
    let x = col(&text, "Workspaces");
    assert_eq!(
        buf[(x, title)].style().fg,
        Some(crate::theme::Role::Accent.color())
    );
    assert!(
        buf[(x, title)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    // Centred across: the ▄ edge's run sits even from both sides, and
    // the title opens past its two blank columns.
    let edge = row_text(&buf, title.saturating_sub(2), 80);
    assert!(edge.contains('▄'));
    let from = edge.find('▄').expect("the edge");
    let run = edge[from..].chars().take_while(|ch| *ch == '▄').count();
    assert!(from > 0 && from == 80 - (from + run), "{edge:?}");
    assert_eq!(usize::from(x), from + 2, "the title opens past the pad");
    // Centred down: the ▄ edge floats below the top and the ▀ edge
    // above the foot.
    assert!(title.saturating_sub(2) > 0);
    let foot = u16::try_from(
        shown
            .lines()
            .position(|row| row.contains("Esc closes"))
            .expect("the legend"),
    )
    .unwrap_or(u16::MAX);
    assert!(foot.saturating_add(2) < 23);
    assert!(row_text(&buf, foot.saturating_add(2), 80).contains('▀'));
    // The first row sits on the bar in `accent` with black text; the
    // second keeps the surface tint.
    let accent = crate::theme::Role::Accent.color();
    let surface = crate::theme::Role::Surface.color();
    let first = title.saturating_add(2);
    assert!(row_text(&buf, first, 80).contains("/w"), "{shown}");
    assert_eq!(buf[(40, first)].bg, accent, "{shown}");
    assert_eq!(
        buf[(40, first)].style().fg,
        Some(crate::look::BAR_TEXT),
        "{shown}"
    );
    let second = first.saturating_add(1);
    assert!(row_text(&buf, second, 80).contains("/repo0"), "{shown}");
    assert_eq!(buf[(40, second)].bg, surface, "{shown}");
    // The legend reads keys bold and labels dim.
    let legend = row_text(&buf, foot, 80);
    let x = col(&legend, "↑↓");
    assert!(
        buf[(x, foot)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD),
        "{legend:?}"
    );
    let x = col(&legend, "move");
    assert!(
        buf[(x, foot)]
            .modifier
            .contains(ratatui::style::Modifier::DIM),
        "{legend:?}"
    );
}

/// Clicking a picker row chooses its workspace and closes the picker.
#[test]
fn clicking_a_picker_row_chooses_its_workspace() {
    let mut app = picking(80, 24);
    // The rows keep their pick targets over the entries shown.
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    let mut picks: Vec<(usize, Rect)> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Pick(at)) = target.id {
                Some((at, target.rect))
            } else {
                None
            }
        })
        .collect();
    picks.sort_by_key(|(at, _)| *at);
    assert_eq!(picks.len(), 2);
    for (at, rect) in &picks {
        let row: String = (rect.x..rect.right())
            .map(|x| buf[(x, rect.y)].symbol().to_owned())
            .collect();
        let want = ["/w", "/repo0"][*at];
        assert!(row.contains(want), "row {at} shows {want}: {row:?}");
    }
    app.on_click(crate::mouse::TargetId::Home(Spot::Pick(1)));
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_none())
    );
    assert_eq!(app.workspace(), Some(PathBuf::from("/repo0")));
}

/// A short home screen with the picker on its last entry: the fit
/// window keeps the selection barred.
#[test]
fn short_picker_keeps_its_selection_barred() {
    let mut app = home(80, 10);
    app.on_line(hello());
    for n in 0..9u8 {
        app.on_line(status_in(
            &format!("s_{n:016x}"),
            "fix the parser",
            &format!("/repo{n}"),
        ));
    }
    open_cleared(&mut app);
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..9 {
        app.on_key(Key::Down, now);
    }
    let buf = buffer(&app, 80, 10);
    let shown = screen(&app, 80, 10);
    let accent = crate::theme::Role::Accent.color();
    let bars: Vec<u16> = (0..10)
        .filter(|y| (0..80).any(|x| buf[(x, *y)].bg == accent))
        .collect();
    assert_eq!(bars.len(), 1, "{shown}");
    let row: String = (0..80)
        .map(|x| buf[(x, bars[0])].symbol().to_owned())
        .collect();
    assert!(row.contains("/repo8"), "{shown}");
    // Two body rows fit the chrome: the selection and the one above.
    // Read inside the bar's columns, past the session list showing
    // through the side margins.
    let from = (0..80)
        .find(|x| buf[(*x, bars[0])].bg == accent)
        .unwrap_or(0);
    let to = (0..80)
        .rev()
        .find(|x| buf[(*x, bars[0])].bg == accent)
        .unwrap_or(0);
    let above: String = (from..=to)
        .map(|x| buf[(x, bars[0].saturating_sub(1))].symbol().to_owned())
        .collect();
    assert!(above.contains("/repo7"), "{shown}");
    let higher: String = (from..=to)
        .map(|x| buf[(x, bars[0].saturating_sub(2))].symbol().to_owned())
        .collect();
    assert!(!higher.contains("/repo"), "{shown}");
}

/// Home at 80x24 with `/` open and the sixth match focused: the fit
/// window keeps the selection barred, counted where it sits.
#[test]
fn short_home_slash_keeps_its_selection_barred() {
    let mut app = home(80, 24);
    type_draft(&mut app, "/");
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..5 {
        app.on_key(Key::Down, now);
    }
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    let accent = crate::theme::Role::Accent.color();
    let selected = (0..24)
        .find(|y| row_text(&buf, *y, 80).contains("credential"))
        .unwrap_or_else(|| panic!("the selected workspace draws:\n{shown}"));
    assert!(
        (0..80).any(|x| buf[(x, selected)].bg == accent),
        "the selected workspace stays barred:\n{shown}"
    );
    let row = row_text(&buf, selected, 80);
    assert!(row.contains("credential"), "{shown}");
    // The range counts matching entries, including every built-in
    // (`docs/tui.md:1682-1685,1995`).
    assert!(shown.contains("of 44"), "{shown}");
}

/// A git home at 80x24 with two live rows in the launch project and no
/// toggle: the chip row, then the rows.
fn focus_home() -> App {
    let mut app = git_home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "first", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "second", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app
}

/// Renders home and checks every chip target's cells: the `tinted`
/// chip carries the lifted surface tint and no reversal, the others
/// neither.
fn check_chips(app: &App, tinted: Option<Spot>) {
    use ratatui::style::Modifier;

    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    let hover = crate::theme::hover_bg(crate::theme::Role::Surface.color());
    for target in &targets {
        let crate::mouse::TargetId::Home(spot) = target.id else {
            continue;
        };
        if !spot.is_chip() {
            continue;
        }
        for x in target.rect.x..target.rect.x.saturating_add(target.rect.width) {
            let cell = &buf[(x, target.rect.y)];
            if tinted == Some(spot) {
                assert_eq!(cell.bg, hover, "the focused chip is tinted");
                assert!(
                    !cell.modifier.contains(Modifier::REVERSED),
                    "the focused chip is never reversed"
                );
            } else {
                assert_ne!(cell.bg, hover, "no other chip is tinted");
                assert!(
                    !cell.modifier.contains(Modifier::REVERSED),
                    "no chip is reversed"
                );
            }
        }
    }
    assert!(
        targets
            .iter()
            .any(|target| matches!(target.id, crate::mouse::TargetId::Home(_))),
        "chips draw targets"
    );
}

#[test]
fn home_focus_entry() {
    let app = focus_home();
    check_chips(&app, None);
    insta::assert_snapshot!("home_focus_entry", screen(&app, 80, 24));
}

#[test]
fn home_focus_chip() {
    let mut app = focus_home();
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
    check_chips(&app, Some(Spot::Workspace));
    let drawn = screen(&app, 80, 24);
    assert!(drawn.contains('█'), "the entry bar keeps its cursor");
    insta::assert_snapshot!("home_focus_chip", drawn);
}

#[test]
fn home_focus_chip_typing() {
    let mut app = focus_home();
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Down, now);
    type_draft(&mut app, "fix");
    check_chips(&app, None);
    insta::assert_snapshot!("home_focus_chip_typing", screen(&app, 80, 24));
}

#[test]
fn home_focus_first_row() {
    let mut app = focus_home();
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    app.on_key(Key::Up, now);
    check_chips(&app, None);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let drawn = screen(&app, 80, 24);
    let at = |name: &str| {
        drawn
            .lines()
            .position(|row| row.contains(name))
            .and_then(|row| u16::try_from(row).ok())
            .unwrap_or_else(|| panic!("{name} draws"))
    };
    assert_eq!(
        buf[(0, at("first"))].bg,
        crate::theme::Role::Selection.color()
    );
    assert_ne!(
        buf[(0, at("second"))].bg,
        crate::theme::Role::Selection.color()
    );
    insta::assert_snapshot!("home_focus_first_row", drawn);
}

/// A git home at 80x24 with hover off and two live rows.
fn focus_home_no_hover() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: true,
        },
        hover: false,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "first", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "second", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app
}

#[test]
fn a_focused_chip_is_tinted_with_hover_off() {
    let mut app = focus_home_no_hover();
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
    check_chips(&app, Some(Spot::Workspace));
}

#[test]
fn a_focused_model_chip_matches_a_hovered_one() {
    let mut app = focus_home();
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Down, now);
    app.on_edit(crate::keys::Edit::Right);
    app.on_edit(crate::keys::Edit::Right);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Model))
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut focused = Buffer::empty(area);
    let targets = render(&app, area, &mut focused, None);
    let rect = targets
        .iter()
        .find_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Model) = target.id {
                Some(target.rect)
            } else {
                None
            }
        })
        .expect("the model chip draws a target");
    // The same frame with the pointer over the model chip: its cells
    // read the same.
    let plain = focus_home();
    let mut hovered = Buffer::empty(area);
    render(&plain, area, &mut hovered, Some((rect.x, rect.y)));
    for x in rect.x..rect.x.saturating_add(rect.width) {
        assert_eq!(
            focused[(x, rect.y)],
            hovered[(x, rect.y)],
            "cell {x} matches a hovered chip"
        );
    }
}

#[test]
fn a_chip_past_the_box_has_no_target() {
    let model = "m".repeat(36);
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some(model.clone()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    });
    app.set_size(40, 24);
    let area = Rect::new(0, 0, 40, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    let chips: Vec<(Spot, ratatui::layout::Rect)> = targets
        .into_iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(spot) = target.id
                && spot.is_chip()
            {
                Some((spot, target.rect))
            } else {
                None
            }
        })
        .collect();
    // The thinking chip starts past the box's right edge: no target.
    assert!(
        chips.iter().all(|(spot, _)| *spot != Spot::Thinking),
        "a chip past the box has no target"
    );
    // The cut chip keeps its visible cells, ending at the box edge.
    let (_, rect) = chips
        .iter()
        .find(|(spot, _)| *spot == Spot::Model)
        .expect("the model chip draws");
    assert_eq!(rect.x.saturating_add(rect.width), 40);
    // → from the last drawn chip stays there.
    app.drawn(&home_only(&app, area, &mut Buffer::empty(area)));
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Down, now);
    app.on_edit(crate::keys::Edit::Right);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Model))
    );
    app.on_edit(crate::keys::Edit::Right);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(Spot::Model))
    );
}

/// Types `text` into the open picker, collecting every `(id, path)` its
/// `directories` lines send.
fn type_path(app: &mut App, text: &str) -> Vec<(String, String)> {
    let now = fakes::clock::FakeClock::new().now();
    let mut out = Vec::new();
    for ch in text.chars() {
        if let Effect::Send(lines) = app.on_key(Key::Char(ch), now) {
            for line in &lines {
                let line: serde_json::Value = serde_json::from_str(line).unwrap();
                if line["command"] == "directories" {
                    out.push((
                        line["id"].as_str().unwrap().to_owned(),
                        line["args"]["path"].as_str().unwrap().to_owned(),
                    ));
                }
            }
        }
    }
    out
}

/// The `(id, path)` of every `directories` line `effect` sends.
fn directories(effect: Effect) -> Vec<(String, String)> {
    let Effect::Send(lines) = effect else {
        return Vec::new();
    };
    lines
        .iter()
        .filter_map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).ok()?;
            (line["command"] == "directories").then(|| {
                (
                    line["id"].as_str().unwrap_or_default().to_owned(),
                    line["args"]["path"].as_str().unwrap_or_default().to_owned(),
                )
            })
        })
        .collect()
}

/// Whether the open picker shows its typed row.
fn typed_open(app: &App) -> bool {
    app.home_screen()
        .and_then(|screen| screen.picker)
        .is_some_and(|picker| picker.typed.is_some())
}

/// Clicks the workspace chip: the picker opens browsing its root.
/// Answers the root's listing with `names`.
fn open_listed(app: &mut App, names: &[&str]) {
    let sent = directories(app.on_click(crate::mouse::TargetId::Home(Spot::Workspace)));
    assert_eq!(sent.len(), 1, "the root's one listing");
    answer(app, &sent[0].0, names, "/w");
}

/// Clicks the workspace chip, then presses Backspace past the root,
/// leaving the recents alone. Returns the parent listings Backspace
/// asked for on the way, in send order.
fn open_cleared(app: &mut App) -> Vec<(String, String)> {
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    let now = fakes::clock::FakeClock::new().now();
    let mut sent = Vec::new();
    while typed_open(app) {
        sent.extend(directories(app.on_key(Key::Backspace, now)));
    }
    sent
}

/// Answers the `directories` request `id` with `names` at `path`.
fn answer(app: &mut App, id: &str, names: &[&str], path: &str) {
    app.on_line(accepted(
        id,
        serde_json::json!({"directories": names, "path": path}),
    ));
}

/// An app with the picker typing `text`: on hello, one live row in
/// `/repo0`, the picker open.
fn typing(width: u16, height: u16, text: &str) -> (App, Vec<(String, String)>) {
    let mut app = home(width, height);
    app.on_line(hello());
    app.on_line(status_in("s_0000000000000000", "fix the parser", "/repo0"));
    let mut sent = open_cleared(&mut app);
    sent.extend(type_path(&mut app, text));
    (app, sent)
}

/// The typed picker at 80x24: `/repo/fi` with two completions over two
/// recents.
#[test]
fn home_picker_typed() {
    let (mut app, sent) = typing(80, 24, "/repo/fi");
    answer(
        &mut app,
        &sent[1].0,
        &["fiber", "fiber-worktrees"],
        "/repo/",
    );
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("› /repo/fi"), "{shown}");
    assert!(shown.contains("fiber-worktrees"), "{shown}");
    assert!(shown.contains("── recents ──"), "{shown}");
    insta::assert_snapshot!("home_picker_typed", shown);
}

/// The typed picker's cells: the typed row's prompt in `info` with a dim
/// cursor, the first completion barred and bold, the header and the
/// unfocused recents dim.
#[test]
fn the_typed_picker_row_styles() {
    use ratatui::style::Modifier;

    let (mut app, sent) = typing(80, 24, "/repo/fi");
    answer(
        &mut app,
        &sent[1].0,
        &["fiber", "fiber-worktrees"],
        "/repo/",
    );
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    let row = |piece: &str| {
        shown
            .lines()
            .position(|line| line.contains(piece))
            .and_then(|row| u16::try_from(row).ok())
            .unwrap_or_else(|| panic!("{piece} draws"))
    };
    let col = |y: u16, piece: &str| {
        let text: String = (0..80).map(|x| buf[(x, y)].symbol().to_owned()).collect();
        let at = text.find(piece).unwrap_or_else(|| panic!("{piece} draws"));
        u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX)
    };
    // The typed row's `› ` reads in `info`.
    let typed = row("› /repo/fi");
    assert_eq!(
        buf[(col(typed, "›"), typed)].style().fg,
        Some(crate::theme::Role::Info.color()),
        "{shown}"
    );
    // Its cursor is dim.
    let cursor = col(typed, "█");
    assert!(
        buf[(cursor, typed)]
            .style()
            .add_modifier
            .contains(Modifier::DIM),
        "{shown}"
    );
    // The first completion sits on the bar, bold.
    let first = row("fiber");
    assert_eq!(
        buf[(col(first, "fiber"), first)].bg,
        crate::theme::Role::Accent.color(),
        "{shown}"
    );
    assert!(
        buf[(col(first, "fiber"), first)]
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
        "{shown}"
    );
    // The header and the unfocused recents stay dim.
    let header = row("── recents ──");
    assert!(
        buf[(col(header, "──"), header)]
            .style()
            .add_modifier
            .contains(Modifier::DIM),
        "{shown}"
    );
    let recent = row("/repo0");
    assert!(
        buf[(col(recent, "/repo0"), recent)]
            .style()
            .add_modifier
            .contains(Modifier::DIM),
        "{shown}"
    );
}

/// The typed row keeps its drawn cursor on a narrow screen: at 40
/// columns a 34-column path fills the overlay's 36 content columns,
/// and the cursor cell still draws after the cut text.
#[test]
fn the_typed_row_keeps_its_cursor_on_a_narrow_screen() {
    let path = format!("/{}", "a".repeat(33));
    assert_eq!(path.chars().count(), 34);
    let (app, _sent) = typing(40, 24, &path);
    let buf = buffer(&app, 40, 24);
    let shown = screen(&app, 40, 24);
    let y = shown
        .lines()
        .position(|line| line.contains("› /a"))
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or_else(|| panic!("the typed row draws: {shown}"));
    let text: String = (0..40).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    let at = text
        .find("█")
        .unwrap_or_else(|| panic!("the cursor draws: {text:?}"));
    let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
    assert_eq!(buf[(x, y)].symbol(), "█", "{shown}");
}

/// A focused recent while typing sits on the bar, and keeps no dim.
#[test]
fn a_focused_recent_while_typing_is_barred() {
    use ratatui::style::Modifier;

    let (mut app, sent) = typing(80, 24, "/repo/fi");
    answer(
        &mut app,
        &sent[1].0,
        &["fiber", "fiber-worktrees"],
        "/repo/",
    );
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    assert_eq!(app.on_key(Key::Down, now), Effect::None);
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    let y = shown
        .lines()
        .position(|line| line.contains("/w"))
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or_else(|| panic!("/w draws: {shown}"));
    let text: String = (0..80).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    let at = text.find("/w").unwrap_or_else(|| panic!("/w draws"));
    let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
    assert_eq!(
        buf[(x, y)].bg,
        crate::theme::Role::Accent.color(),
        "{shown}"
    );
    assert!(
        !buf[(x, y)].style().add_modifier.contains(Modifier::DIM),
        "{shown}"
    );
}

/// "no such directory" draws dim with no completion rows, above the
/// blank row, the header and the recents.
#[test]
fn the_missing_row_draws_dim_over_the_recents() {
    use ratatui::style::Modifier;

    let (mut app, sent) = typing(80, 24, "/repo/zz");
    answer(&mut app, &sent[1].0, &["fiber"], "/repo/");
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    let buf = buffer(&app, 80, 24);
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("no such directory"), "{shown}");
    assert!(!shown.contains("fiber"), "{shown}");
    let missing = shown
        .lines()
        .position(|line| line.contains("no such directory"))
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or_else(|| panic!("the missing row draws"));
    let text: String = (0..80)
        .map(|x| buf[(x, missing)].symbol().to_owned())
        .collect();
    let at = text
        .find("no such directory")
        .unwrap_or_else(|| panic!("the missing row draws"));
    let x = u16::try_from(text[..at].chars().count()).unwrap_or(u16::MAX);
    assert!(
        buf[(x, missing)]
            .style()
            .add_modifier
            .contains(Modifier::DIM),
        "{shown}"
    );
    // The blank row stands the header off the missing row, and the
    // recents follow it.
    let lines: Vec<&str> = shown.lines().collect();
    let header = lines
        .iter()
        .position(|line| line.contains("── recents ──"))
        .unwrap_or_else(|| panic!("the header draws: {shown}"));
    assert!(lines[header - 1].trim().is_empty(), "{shown}");
    assert!(lines[header + 1].contains("/w"), "{shown}");
}

/// The typed picker's body rows within `room` rows, with 30 completions
/// and 3 recents: the typed row and the focused row always draw, and
/// the separators yield first.
#[test]
fn the_typed_body_keeps_the_typed_row_and_the_focus() {
    use TypedRow::{Blank, Completion as C, Header, Recent as R, Typed as T};
    let body = |room: usize, focus: Option<usize>| typed_body(30, 3, focus, false, room);
    // A focused completion draws with the typed row, windowed around it.
    assert_eq!(body(2, Some(0)), vec![T, C(0)]);
    assert_eq!(body(5, Some(0)), vec![T, C(0), C(1), C(2), C(3)]);
    assert_eq!(body(2, Some(29)), vec![T, C(29)]);
    assert_eq!(body(3, Some(29)), vec![T, C(28), C(29)]);
    assert_eq!(body(4, Some(29)), vec![T, C(27), C(28), C(29)]);
    assert_eq!(body(5, Some(29)), vec![T, C(26), C(27), C(28), C(29)]);
    // A focused recent stands without the separators when neither fits.
    assert_eq!(body(2, Some(30)), vec![T, R(0)]);
    assert_eq!(body(2, Some(32)), vec![T, R(2)]);
    // With room for one more row, the next recent joins it, else the
    // nearest completion.
    assert_eq!(body(3, Some(30)), vec![T, R(0), R(1)]);
    assert_eq!(body(3, Some(32)), vec![T, C(29), R(2)]);
    // From room 4 the separators draw with the focused recent.
    assert_eq!(body(4, Some(30)), vec![T, Blank, Header, R(0)]);
    assert_eq!(body(4, Some(32)), vec![T, Blank, Header, R(2)]);
    assert_eq!(body(5, Some(30)), vec![T, Blank, Header, R(0), R(1)]);
    assert_eq!(body(5, Some(32)), vec![T, Blank, Header, R(1), R(2)]);
    // Nothing focused anchors the window at the first row.
    assert_eq!(body(3, None), vec![T, C(0), C(1)]);
    // Every body holds at most its room.
    for room in 0..8usize {
        for focus in [None, Some(0), Some(15), Some(29), Some(30), Some(32)] {
            assert!(
                body(room, focus).len() <= room.max(1),
                "room {room} focus {focus:?}"
            );
        }
    }
}

/// The typed body with no completions: the typed row, then the recents
/// with their separators once those fit.
#[test]
fn the_typed_body_without_completions() {
    use TypedRow::{Blank, Header, Missing, Recent as R, Typed as T};
    assert_eq!(
        typed_body(0, 3, None, false, 5),
        vec![T, Blank, Header, R(0), R(1)]
    );
    assert_eq!(typed_body(0, 3, None, false, 3), vec![T]);
    assert_eq!(
        typed_body(0, 2, None, true, 6),
        vec![T, Missing, Blank, Header, R(0), R(1)]
    );
    assert_eq!(
        typed_body(0, 2, Some(1), true, 4),
        vec![T, Blank, Header, R(1)]
    );
}

/// With no recents, the completions alone draw, and nothing focused
/// past them finds a recent to stand on.
#[test]
fn the_typed_body_without_recents_draws_only_the_completions() {
    use TypedRow::{Completion as C, Typed as T};
    assert_eq!(typed_body(2, 0, Some(2), false, 6), vec![T, C(0), C(1)]);
    assert_eq!(typed_body(5, 0, Some(5), false, 3), vec![T, C(0), C(1)]);
    assert_eq!(typed_body(5, 0, Some(7), false, 3), vec![T, C(0), C(1)]);
    assert_eq!(typed_body(0, 0, None, false, 6), vec![TypedRow::Typed]);
}

/// A focused recent with no completions: the missing row draws beside
/// it, under the separators.
#[test]
fn the_typed_body_draws_the_missing_row_beside_a_focused_recent() {
    use TypedRow::{Blank, Header, Missing, Recent as R, Typed as T};
    assert_eq!(
        typed_body(0, 1, Some(0), true, 6),
        vec![T, Missing, Blank, Header, R(0)]
    );
}

/// A focused recent with completions: the nearest completions draw
/// above the separators, beside it.
#[test]
fn the_typed_body_draws_the_completions_beside_a_focused_recent() {
    use TypedRow::{Blank, Completion as C, Header, Recent as R, Typed as T};
    assert_eq!(
        typed_body(2, 1, Some(2), false, 8),
        vec![T, C(0), C(1), Blank, Header, R(0)]
    );
}

/// A focused recent in the budget-two row with no completions: the
/// focused recent alone, with no completion row added.
#[test]
fn the_typed_body_beside_a_focused_recent_without_completions() {
    use TypedRow::{Recent as R, Typed as T};
    assert_eq!(typed_body(0, 2, Some(1), false, 3), vec![T, R(1)]);
}

/// A short screen keeps the focused completion barred: the last of 30
/// draws with the typed row, and the first drops out.
#[test]
fn a_short_screen_keeps_the_focused_completion_drawn() {
    let mut app = home(80, 12);
    app.on_line(hello());
    let mut sent = open_cleared(&mut app);
    sent.extend(type_path(&mut app, "/"));
    let mut all = Vec::new();
    for n in 0..30 {
        all.push(format!("c{n:02}"));
    }
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    answer(&mut app, &sent[0].0, &refs, "/");
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..29 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let buf = buffer(&app, 80, 12);
    let shown = screen(&app, 80, 12);
    // Room 4 holds the typed row and the window ending at the focus.
    assert!(shown.contains("› /"), "{shown}");
    assert!(shown.contains("c29"), "{shown}");
    assert!(!shown.contains("c00"), "{shown}");
    let accent = crate::theme::Role::Accent.color();
    let bars: Vec<u16> = (0..12)
        .filter(|y| (0..80).any(|x| buf[(x, *y)].bg == accent))
        .collect();
    assert_eq!(bars.len(), 1, "{shown}");
    let row: String = (0..80)
        .map(|x| buf[(x, bars[0])].symbol().to_owned())
        .collect();
    assert!(row.contains("c29"), "{shown}");
}

/// A short screen keeps the focused recent drawn with the typed row,
/// dropping the completions and the separators first.
#[test]
fn a_short_screen_keeps_the_focused_recent_drawn() {
    let mut app = home(80, 10);
    app.on_line(hello());
    let mut sent = open_cleared(&mut app);
    sent.extend(type_path(&mut app, "/a"));
    let mut all = Vec::new();
    for n in 0..30 {
        all.push(format!("a{n:02}"));
    }
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    answer(&mut app, &sent[0].0, &refs, "/");
    // Past the 30 completions to the first recent.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..31 {
        assert_eq!(app.on_key(Key::Down, now), Effect::None);
    }
    let shown = screen(&app, 80, 10);
    // Room 2 holds the typed row and the focused recent alone.
    assert!(shown.contains("› /a"), "{shown}");
    assert!(shown.contains("/w"), "{shown}");
    assert!(!shown.contains("a00"), "{shown}");
    assert!(!shown.contains("── recents ──"), "{shown}");
}

/// With the link down and the picker open, home's frame holds the
/// reconnect banner; with the link up it does not.
#[test]
fn the_reconnect_banner_draws_over_home() {
    let (app, _) = typing(80, 24, "/");
    let up = screen(&app, 80, 24);
    assert!(!up.contains("reconnecting"), "{up}");
    let mut down = app;
    down.connect_failed("Could not reach the hub: refused".to_owned());
    down.next_retry();
    let shown = screen(&down, 80, 24);
    assert!(shown.contains("Connection lost · reconnecting"), "{shown}");
    // The foot hint stays on the last row; at 80 columns only its
    // start shows.
    let last = shown.lines().last().unwrap_or_default();
    assert!(last.contains("Enter opens the focused session"), "{shown}");
}

/// A recents-only picker still parks the cursor in the draft; with the
/// typed row open the terminal cursor hides.
#[test]
fn the_cursor_hides_while_typing_a_path() {
    let mut app = picking_recents(80, 24);
    let area = Rect::new(0, 0, 80, 24);
    let screen = app.home_screen().expect("home draws");
    assert!(super::cursor(&app, &screen, area).is_some());
    type_path(&mut app, "/");
    let screen = app.home_screen().expect("home draws");
    assert!(
        screen
            .picker
            .as_ref()
            .is_some_and(|picker| picker.typed.is_some())
    );
    assert_eq!(super::cursor(&app, &screen, area), None);
}

/// While typing, only the recent rows take clicks, by their index into
/// the recents: completion, typed, blank and header rows give no target.
#[test]
fn clicking_a_recent_while_typing_chooses_it() {
    let (mut app, sent) = typing(80, 24, "/repo/f");
    answer(&mut app, &sent[1].0, &["fiber"], "/repo/");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = home_only(&app, area, &mut buf);
    let mut picks: Vec<usize> = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Home(Spot::Pick(at)) = target.id {
                Some(at)
            } else {
                None
            }
        })
        .collect();
    picks.sort();
    // The two recents, and nothing else.
    assert_eq!(picks, vec![0, 1]);
    app.on_click(crate::mouse::TargetId::Home(Spot::Pick(1)));
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.picker.is_none())
    );
    assert_eq!(app.workspace(), Some(PathBuf::from("/repo0")));
}

/// Home drawn and painted without colour, with its targets.
fn no_color_home(app: &App, width: u16, height: u16) -> (Buffer, Vec<crate::mouse::Target>) {
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = home_only(app, area, &mut buf);
    let (look, notice) = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name: &str| {
        (name == "NO_COLOR").then(|| "1".to_owned())
    });
    assert_eq!(notice, None);
    look.paint(&mut buf);
    (buf, targets)
}

#[test]
fn no_color_boxes_the_home_box_and_narrows_its_text() {
    use ratatui::style::{Color, Modifier};
    let mut app = home(80, 24);
    type_draft(&mut app, "hello");
    let (buf, targets) = no_color_home(&app, 80, 24);
    let shown: Vec<String> = (0..24)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    // The box is 80 wide: ┌─┐ above the draft, └─┘ below the chips,
    // dim, on the default colours.
    let top = u16::try_from(
        shown
            .iter()
            .position(|row| row.starts_with('┌'))
            .expect("the box's top edge"),
    )
    .expect("a row");
    let bottom = u16::try_from(
        shown
            .iter()
            .position(|row| row.starts_with('└'))
            .expect("the box's bottom edge"),
    )
    .expect("a row");
    assert!(bottom > top);
    for y in [top, bottom] {
        for x in 0..80u16 {
            let cell = &buf[(x, y)];
            assert_eq!(
                (cell.fg, cell.bg),
                (Color::Reset, Color::Reset),
                "({x}, {y})"
            );
            assert_eq!(cell.modifier, Modifier::DIM, "({x}, {y})");
        }
    }
    assert_eq!(shown[usize::from(top)].chars().last().unwrap(), '┐');
    assert_eq!(shown[usize::from(bottom)].chars().last().unwrap(), '┘');
    // The draft and the chips sit one column in, between │ sides, with
    // the drawn cursor on the interior.
    for y in (top + 1)..bottom {
        assert_eq!(buf[(0, y)].symbol(), "│", "row {y}");
        assert_eq!(buf[(79, y)].symbol(), "│", "row {y}");
    }
    let draft = &shown[usize::from(top) + 1];
    assert!(draft.contains("hello"), "{draft:?}");
    assert!(draft.starts_with("│ ›"), "{draft:?}");
    let caret: String = (0..80u16).map(|x| buf[(x, top + 1)].symbol()).collect();
    assert!(caret.contains('█'), "{caret:?}");
    assert!(!caret.ends_with('█'), "the cursor never sits on a border");
    // Chip targets keep inside the interior, clear of the sides.
    for target in &targets {
        assert!(target.rect.x >= 1, "{target:?}");
        assert!(target.rect.right() <= 79, "{target:?}");
    }
    // The view's cursor agrees with the drawn one, one column in.
    let cursor = cursor(&app, Rect::new(0, 0, 80, 24)).expect("the cursor");
    assert_eq!(buf[(cursor.x, cursor.y)].symbol(), "█");
    assert!((1..79).contains(&cursor.x));
}

#[test]
fn no_color_home_navigation_follows_the_rendered_rows() {
    // A draft wrapping to three rows at the boxed interior: Up moves to
    // the end of the row above, Down returns, per the rendered rows.
    crate::surface::init(&|name: &str| (name == "NO_COLOR").then(|| "1".to_owned()));
    let mut app = home(40, 24);
    type_draft(&mut app, &"w".repeat(100));
    let area = Rect::new(0, 0, 40, 24);
    let before = cursor(&app, area).expect("the cursor");
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::Up, now);
    let up = cursor(&app, area).expect("the cursor");
    assert!(
        up.y < before.y || (up.y == before.y && up.x < before.x),
        "{before:?} -> {up:?}"
    );
    app.on_key(Key::Down, now);
    assert_eq!(cursor(&app, area).expect("the cursor"), before);
    // The draft width is the boxed interior: navigation counts the rows
    // the renderer wraps.
    assert_eq!(app.draft_width(), 38);
    let (buf, _) = no_color_home(&app, 40, 24);
    let box_top = u16::try_from(
        (0..24u16)
            .map(|y| (0..40u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .position(|row| row.starts_with('┌'))
            .expect("the box"),
    )
    .expect("a row");
    assert_eq!(buf[(0, box_top)].symbol(), "┌");
    assert_eq!(buf[(39, box_top)].symbol(), "┐");
}

/// The panel slab's bounds: the ▄ edge above the title and the ▀ edge
/// below the footer. Home's own box edges share those rows, so the
/// panel's edge is the run covering the screen's middle column.
fn panel_slab(shown: &str, middle: usize) -> (usize, usize, usize, usize) {
    /// A run of `edge` covering `middle`, if any.
    fn run(row: &str, edge: char, middle: usize) -> Option<(usize, usize)> {
        let cells: Vec<char> = row.chars().collect();
        let mut at = 0;
        while at < cells.len() {
            if cells[at] == edge {
                let mut end = at;
                while end < cells.len() && cells[end] == edge {
                    end += 1;
                }
                if at <= middle && middle < end {
                    return Some((at, end - at));
                }
                at = end;
            } else {
                at += 1;
            }
        }
        None
    }
    let rows: Vec<&str> = shown.lines().collect();
    let title = rows
        .iter()
        .position(|row| row.contains("Prompt history"))
        .expect("the panel title");
    let (top, (x0, wide)) = (0..title)
        .rev()
        .filter_map(|y| run(rows[y], '▄', middle).map(|run| (y, run)))
        .next()
        .expect("the top edge");
    let (bottom, _) = (title..rows.len())
        .filter_map(|y| run(rows[y], '▀', middle).map(|run| (y, run)))
        .next()
        .expect("the bottom edge");
    (x0, top, wide, bottom + 1 - top)
}

#[test]
fn the_search_panel_centres_over_homes_area() {
    let mut app = home(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlR, now);
    assert!(app.completions().is_some());
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("Prompt history"), "{shown}");
    assert!(shown.contains("no matching prompts"), "{shown}");
    // Centred across and down home's whole area, as the quit question is.
    let (x0, y0, wide, high) = panel_slab(&shown, 40);
    assert_eq!(x0, (80 - wide) / 2, "{shown}");
    assert_eq!(y0, (24 - high) / 2, "{shown}");
}

#[test]
fn the_search_panel_gives_way_on_a_short_home() {
    let mut app = home(80, 24);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::CtrlR, now);
    assert!(app.completions().is_some());
    // Nine rows leave the query row alone: the title and query stay,
    // the no-match row gives way, and no entry row clips.
    let area = Rect::new(0, 0, 80, 9);
    let mut buf = Buffer::empty(area);
    home_only(&app, area, &mut buf);
    let shown = crate::view::text(&buf);
    assert!(shown.contains("Prompt history"), "{shown}");
    assert!(shown.contains("›"), "{shown}");
    assert!(!shown.contains("no matching prompts"), "{shown}");
}

/// The rows of the typed picker's `Workspaces` title and footer, and of
/// the input box's top edge, on a home `height` rows tall with the
/// picker browsing `/w/` over two completions and the `/w` recent: a
/// picker 14 rows tall.
fn typed_picker_rows(height: u16) -> (usize, usize, usize) {
    let mut app = home(80, height);
    app.on_line(hello());
    open_listed(&mut app, &["a", "b"]);
    let shown = screen(&app, 80, height);
    let lines: Vec<&str> = shown.lines().collect();
    let find = |piece: &str| {
        lines
            .iter()
            .position(|line| line.contains(piece))
            .unwrap_or_else(|| panic!("{piece} draws: {shown}"))
    };
    let title = find("Workspaces");
    let footer = find("↑↓ move · Enter open");
    let box_top = find("▌ › ").saturating_sub(1);
    (title, footer, box_top)
}

/// With room above the box exactly the picker's height, the picker
/// fills it: its top edge on the first row, its bottom edge on the row
/// above the box.
#[test]
fn a_picker_exactly_as_tall_as_the_room_above_the_box_sits_above_it() {
    let (title, footer, box_top) = typed_picker_rows(20);
    assert_eq!(box_top, 14, "the room above the box is the picker's height");
    // The top edge, then a pad row, then the title.
    assert_eq!(title, 2);
    // The footer, its pad row, then the bottom edge on the row above the box.
    assert_eq!(footer + 2, box_top - 1);
}

/// One row short of the picker's height above the box, the picker
/// stays centred over home, reaching over the box.
#[test]
fn a_picker_one_row_taller_than_the_room_above_the_box_stays_centred() {
    let (title, footer, box_top) = typed_picker_rows(19);
    assert_eq!(box_top, 13, "the room above the box is one row short");
    // Centred in 19 rows: the 14-row picker starts on row 2.
    assert_eq!(title, 4);
    assert_eq!(footer + 2, 15);
    assert!(footer + 2 >= box_top, "the centred picker reaches the box");
}

/// The import offer's frame body rows: the head plus one row per source
/// with its key. Rendering them pins the builder against an empty one.
#[test]
fn import_rows_lists_both_sources_with_their_keys() {
    let area = Rect::new(0, 0, 80, 24);
    let rows = super::import_rows(&crate::home::import_head(), area);
    let text: Vec<String> = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    assert!(!rows.is_empty(), "the offer draws rows");
    assert!(
        text.iter().any(|line| line.contains("Bring settings")),
        "the head draws"
    );
    assert!(
        text.iter()
            .any(|line| line.contains('1') && line.contains("Claude Code")),
        "Claude Code draws with its key"
    );
    assert!(
        text.iter()
            .any(|line| line.contains('2') && line.contains("Codex")),
        "Codex draws with its key"
    );
    assert!(
        text.iter().any(|line| line.contains("~/.claude")),
        "the Claude Code home draws"
    );
    assert!(
        text.iter().any(|line| line.contains("~/.codex")),
        "the Codex home draws"
    );
}

/// The import offer over home: the frame draws its title, the head, one
/// legend row per source and its footer.
#[test]
fn the_import_offer_draws_the_question_sources_and_footer() {
    let mut app = home(80, 24);
    app.set_import_offer(true);
    let drawn = screen(&app, 80, 24);
    assert!(drawn.contains("Import settings"), "{drawn}");
    assert!(drawn.contains("Bring settings"), "{drawn}");
    assert!(drawn.contains("Claude Code"), "{drawn}");
    assert!(drawn.contains("Codex"), "{drawn}");
    assert!(drawn.contains("not now"), "{drawn}");
}

/// The offer's swallow target exists exactly while the frame draws: one
/// slab-wide target over every frame row, and none when the area holds
/// no slab.
#[test]
fn the_import_offer_swallow_target_needs_a_drawn_slab() {
    let question = crate::home::import_head();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_import_question(&question, area, &mut buf, &mut targets);
    assert!(
        targets
            .iter()
            .any(|target| target.id == crate::mouse::TargetId::Home(crate::home::Spot::Question)),
        "the drawn offer swallows clicks behind it"
    );
    let area = Rect::new(0, 0, 80, 0);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_import_question(&question, area, &mut buf, &mut targets);
    assert!(
        !targets
            .iter()
            .any(|target| target.id == crate::mouse::TargetId::Home(crate::home::Spot::Question)),
        "no slab, no swallow target"
    );
}

/// A drag over the import offer copies its row's text: the overlay
/// reads as its own surface with the legend left out (`docs/tui.md`,
/// "Selection and copy").
#[test]
fn a_drag_over_the_import_offer_copies_its_row() {
    use crate::keys::{Button, Mouse, MouseKind};
    use crate::mouse::Pointer;
    let mut app = home(80, 24);
    app.set_import_offer(true);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    let shown = text(&buf);
    let (y, line) = shown
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("Claude Code"))
        .map(|(y, line)| (u16::try_from(y).unwrap_or(u16::MAX), line.to_owned()))
        .expect("the import offer lists Claude Code");
    let x = u16::try_from(line.find("Claude").unwrap_or_default()).unwrap_or(u16::MAX);
    let mut pointer = Pointer::default();
    let now = fakes::clock::FakeClock::new().now();
    let mut report = |kind: MouseKind, col: u16, row: u16| {
        let mut buf = Buffer::empty(area);
        let targets = render(&app, area, &mut buf, None);
        app.on_mouse(&Mouse { kind, col, row }, &targets, &mut pointer, true, now)
    };
    let _ = report(MouseKind::Press(Button::Left), x, y);
    let _ = report(MouseKind::Drag(Button::Left), x + 5, y);
    assert_eq!(
        report(MouseKind::Release, x + 5, y),
        Effect::Copy("Claude".to_owned())
    );
}
