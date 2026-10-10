//! Home's frame: snapshots and the cursor.

use super::super::{cursor, render, text};
use crate::app::{App, QUIT_HINT};
use crate::home::{Launch, Spot};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    home_with_glyph(width, height, "⌇")
}

/// An app on home at `width` by `height` with the one-row logo's `glyph`.
fn home_with_glyph(width: u16, height: u16, glyph: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
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
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: true,
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

/// Chooses a model for this session only: with no model on the chip,
/// Enter opens the picker instead of starting, so the blocker tests
/// pick through it, keeping the `[no model]` chips their snapshots show.
fn choose_session_model(app: &mut App) {
    let now = fakes::clock::FakeClock::new().now();
    assert_eq!(app.on_key(Key::CtrlL, now), crate::app::Effect::None);
    app.on_models(Ok(crate::catalogue::Catalogue {
        models: vec![crate::catalogue::ModelEntry {
            reference: "test/model".to_owned(),
            provider: "test".to_owned(),
            id: "model".to_owned(),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
        }],
        notices: Vec::new(),
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
    for (height, drawn) in [(6, true), (5, false)] {
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
            assert_eq!(at, vec![(3, 5)], "height {height}");
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
    assert!(!text.contains("  g"), "{text}");
    assert!(!text.contains("  h"), "{text}");
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
    assert!(
        app.home_screen()
            .is_some_and(|screen| screen.foot == QUIT_HINT)
    );
    insta::assert_snapshot!("home_quit_hint", screen(&app, 80, 24));
}

#[test]
fn home_cursor_sits_at_the_draft_cursor_in_the_box() {
    let mut app = home(80, 24);
    type_draft(&mut app, "hi");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render(&app, area, &mut buf, None);
    // The pad is three rows, then the four-row logo and one blank row,
    // the ▄ edge and the draft's first row; the cursor sits after "› hi".
    assert_eq!(cursor(&app, area), Some(Position::new(4, 9)));
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
    for x in [0, 1] {
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
    assert_eq!(buf[(2, row)].symbol(), "█");
    assert!(
        buf[(3, row)]
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
    assert_eq!(app.home_screen().map(|screen| screen.rows.len()), Some(15));
    insta::assert_snapshot!("home_with_more_rows_than_fit", screen(&app, 80, 24));
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

/// A live `session_status` for `session`, named `name`, in `state`.
fn status(session: &str, name: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": name,
        "workspace": "/w",
        "project": "-w",
        "since": 0,
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
        "since": 0,
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
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    let now = fakes::clock::FakeClock::new().now();
    // Ten entries, fewer rows above the box: every selection draws,
    // scrolling the list around it. A picker row shares its row with
    // the frame's pad and gutter, so the entry reads past them.
    for selected in 0..10usize {
        let entry = app
            .home_screen()
            .and_then(|screen| screen.picker)
            .and_then(|(list, at)| (at == selected).then(|| list[selected].clone()))
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
fn home_four_row_logo_80x24() {
    insta::assert_snapshot!("home_four_row_logo_80x24", screen(&home(80, 24), 80, 24));
}

#[test]
fn home_one_row_logo_at_the_height_limit() {
    insta::assert_snapshot!(
        "home_one_row_logo_at_the_height_limit",
        screen(&home(80, 16), 80, 16)
    );
}

#[test]
fn the_four_row_logo_needs_its_height_exactly() {
    // At 80 columns the threshold is 17 rows: a pad of 2, the four-row
    // logo, one blank row, the six-row box, three list rows, the foot.
    let tall = screen(&home(80, 17), 80, 17);
    // The four pixel rows sit under the pad of 2; the drawn cursor in
    // the box below must not stand in for them.
    assert!(
        tall.lines().skip(2).take(4).any(|row| row.contains('█')),
        "four pixel rows at the threshold"
    );
    let short = screen(&home(80, 16), 80, 16);
    assert!(
        short.lines().take(3).all(|row| !row.contains('█')),
        "one row below the threshold"
    );
    assert!(short.contains("⌇ fiber 0.0.1"), "the one-row logo");
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
fn the_logo_glyph_setting_changes_the_one_row_logo() {
    // The four-row logo's wave is drawn pixels, and never changes.
    let tall = screen(&home_with_glyph(80, 24, "≈"), 80, 24);
    assert!(tall.contains('█'), "four pixel rows");
    assert!(!tall.contains('≈'), "no glyph in the pixel logo");
    let short = screen(&home_with_glyph(80, 16, "≈"), 80, 16);
    assert!(short.contains("≈ fiber 0.0.1"), "the setting's glyph");
    assert!(!short.contains('⌇'), "no wave where the glyph goes");
}

#[test]
fn home_chips() {
    // The workspace, the model, the thinking level, and what Enter does.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
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

#[test]
fn home_picker_open() {
    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "fix the parser", live()));
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    insta::assert_snapshot!("home_picker_open", screen(&app, 80, 24));
}

/// A waiting `session_status` for `session` in another project.
fn away_waiting() -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "away",
            "workspace": "/lens",
            "project": "-other",
            "state": "waiting",
            "waiting": {"request_id": "r_1", "kind": "approval",
                "summary": "shell"},
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn home_scoped_with_the_toggle() {
    let mut app = git_home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "here", idle()));
    app.on_line(away_waiting());
    insta::assert_snapshot!("home_scoped_with_the_toggle", screen(&app, 80, 24));
}

#[test]
fn home_with_blocker_lines() {
    // A rejected `start` draws its message above the box, wrapped at its
    // width.
    let mut app = home(80, 24);
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
            ("command_id".to_owned(), serde_json::Value::String(id.to_owned())),
            ("code".to_owned(), serde_json::Value::String("start_failed".to_owned())),
            (
                "message".to_owned(),
                serde_json::Value::String(
                    "No API key for test/model in this workspace. Run `fiber login` and press Enter to try again, or pick another model.\nA second line stays short."
                        .to_owned(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    }));
    insta::assert_snapshot!("home_with_blocker_lines", screen(&app, 80, 24));
}

#[test]
fn blockers_count_against_the_four_row_logo() {
    // Three blocker lines at the 17-row threshold leave no room for the
    // pixel logo: the one-row form draws, with the blockers above the
    // box.
    let mut app = home(80, 17);
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
    let text = screen(&app, 80, 17);
    // The pad of 2 and the one logo row hold no pixel rows while the
    // blockers show.
    assert!(
        text.lines().take(3).all(|row| !row.contains('█')),
        "one row while the blockers show"
    );
    assert!(text.contains("⌇ fiber 0.0.1"), "the one-row logo");
    assert_eq!(
        app.home_screen()
            .map(|screen| screen.blockers)
            .unwrap_or_default(),
        ["one", "two", "three"]
    );
}

#[test]
fn the_toggle_keeps_a_row_for_the_list() {
    // Nine rows fit under the box: with the toggle heading the list,
    // eight session rows draw.
    let mut app = git_home(80, 24);
    app.on_line(hello());
    for n in 0..10u8 {
        let session = format!("s_{n:016x}");
        app.on_line(status(&session, "here", idle()));
    }
    app.on_line(away_waiting());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let toggles = targets
        .iter()
        .filter(|target| target.id == crate::mouse::TargetId::Home(Spot::Toggle))
        .count();
    let entries = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Home(Spot::Entry(_))))
        .count();
    assert_eq!(toggles, 1);
    assert_eq!(entries, 8);
}

#[test]
fn home_cursor_hides_while_navigating() {
    let mut app = home(80, 24);
    // Pasted text past the token line count becomes one paste token, a
    // click target focus can move to.
    let pasted: Vec<String> = (1..=312).map(|n| format!("line {n}")).collect();
    app.on_edit(crate::keys::Edit::Paste(pasted.join("\n")));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(Key::BackTab, now);
    assert!(app.focused().is_some());
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
    // Nine rows fit under the four-row logo's box, each with its ✕:
    // nineteen steps focus the tenth row below the fold.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..19 {
        app.on_key(Key::Down, now);
    }
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Entry(
            app.home_screen()
                .and_then(|screen| screen.rows.get(9).map(|(key, _, _)| *key))
                .unwrap_or_else(|| panic!("a tenth row"))
        )))
    );
    let keys: Vec<u64> = app
        .home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default();
    assert_eq!(keys.len(), 15);
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
    assert_eq!(drawn.len(), 9);
    assert_eq!(drawn.last(), Some(&keys[9]));
    assert!(!drawn.contains(&keys[0]));
    // One step reaches the row's ✕, and the list still ends there.
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app.on_key(Key::Down, now);
    assert_eq!(
        app.focused(),
        Some(crate::mouse::TargetId::Home(crate::home::Spot::Stop(
            keys[9]
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
    assert_eq!(drawn.last(), Some(&keys[9]));
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
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
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
        .and_then(|screen| screen.rows.into_iter().next().map(|(key, _, _)| key))
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
    // More wrapped rows than fit: the first id draws, the last does not.
    let first = screen(&app, 80, 14);
    assert!(first.contains("s_0000000000000001"), "the first id draws");
    assert!(
        !first.contains("s_0000000000000014"),
        "the last id is past the screen"
    );
    // Down scrolls until every id can be seen before Enter.
    let now = fakes::clock::FakeClock::new().now();
    for _ in 0..20 {
        app.on_key(Key::Down, now);
    }
    let last = screen(&app, 80, 14);
    assert!(last.contains("s_0000000000000014"), "the last id draws");
    // Up scrolls back to the first rows.
    for _ in 0..20 {
        app.on_key(Key::Up, now);
    }
    let back = screen(&app, 80, 14);
    assert!(back.contains("s_0000000000000001"), "the first id draws");
    assert!(
        !back.contains("s_0000000000000014"),
        "the last id is past the screen again"
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
fn a_wide_screen_centres_the_box() {
    // The box is 100 columns wide on a 120-column screen: ten columns
    // of pad on each side.
    let app = home(120, 24);
    let area = Rect::new(0, 0, 120, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    let workspace = targets
        .iter()
        .find(|target| target.id == crate::mouse::TargetId::Home(Spot::Workspace))
        .unwrap_or_else(|| panic!("a workspace chip target"));
    assert_eq!(workspace.rect.x, 10);
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
    assert_eq!(workspace.rect.x, 0);
    let drawn = screen(&app, 80, 24);
    assert!(
        drawn.lines().any(|row| row.contains(
            "[w]  [no model]  [thinking: default]  enter starts a session"
        )),
        "the chips join with two spaces"
    );
}

#[test]
fn the_toggle_does_not_draw_on_the_foot_row() {
    // At ten rows the list has no room past the box: the toggle shows
    // in state, but draws nothing, and the foot hint keeps its row.
    let mut app = git_home(80, 10);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "here", idle()));
    app.on_line(away_waiting());
    assert!(app.home_screen().and_then(|screen| screen.toggle).is_some());
    let area = Rect::new(0, 0, 80, 10);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::Home(Spot::Toggle)),
        "no toggle target on the foot row"
    );
    assert!(screen(&app, 80, 10).contains("↓ the session list"));
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
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
    assert_eq!(
        app.home_screen()
            .and_then(|screen| screen.picker.map(|(_, selected)| selected)),
        Some(0)
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
    assert!(completions.lines.len() > 1);
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
    assert_eq!(cell(at("Goes home."), "home"), accent, "{drawn}");
    assert_eq!(cell(at("Goes home with"), "new"), surface, "{drawn}");
}

#[test]
fn the_focused_row_is_reversed_and_others_are_not() {
    use ratatui::style::Modifier;

    let mut app = home(80, 24);
    app.on_line(hello());
    app.on_line(status("s_aaaaaaaaaaaaaaaa", "first", live()));
    app.on_line(status("s_bbbbbbbbbbbbbbbb", "second", idle()));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = render(&app, area, &mut buf, None);
    app.drawn(&targets);
    app.on_key(Key::Down, fakes::clock::FakeClock::new().now());
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
    assert!(buf[(0, at("first"))].modifier.contains(Modifier::REVERSED));
    assert!(!buf[(0, at("second"))].modifier.contains(Modifier::REVERSED));
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
    // "> [Pasted text #1 · 312 lines]" on the draft's first row: the pad
    // is three rows, then the four-row logo and one blank row, the ▄
    // edge and the draft.
    let tokens: Vec<ratatui::layout::Rect> = targets
        .iter()
        .filter(|target| matches!(target.id, crate::mouse::TargetId::Token(_)))
        .map(|target| target.rect)
        .collect();
    assert_eq!(tokens, vec![Rect::new(2, 9, 28, 1)]);
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
/// over two recent workspaces.
fn picking(width: u16, height: u16) -> App {
    let mut app = home(width, height);
    app.on_line(hello());
    app.on_line(status_in("s_0000000000000000", "fix the parser", "/repo0"));
    app.on_click(crate::mouse::TargetId::Home(Spot::Workspace));
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
    let app = picking(80, 24);
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
            .position(|row| row.contains("esc closes"))
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
    assert_eq!(app.workspace(), PathBuf::from("/repo0"));
}
