//! The quit question's drawing: the overlay over a session and on home,
//! its count line, its choices and their clicks.

use super::draw;
use crate::app::{App, Effect};
use crate::home::{Launch, QuitChoice, Spot};
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::path::PathBuf;

/// An app on home at `width` by `height`.
fn home(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
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

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// A live `session_status` for `session` with `clients`.
fn live(session: &str, clients: u32) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model",
            "delegates": 0,
            "jobs": 0,
            "clients": clients,
            "state": "streaming",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Opens the quit question with two sessions working.
fn ask_quit(app: &mut App) {
    app.on_line(hello());
    app.on_line(live("s_aaaaaaaaaaaaaaaa", 0));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", 0));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.quit_open());
}

/// Renders `app` at `width` by `height` as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn quit_over_a_session() {
    let mut app = home(160, 48);
    app.on_line(hello());
    app.on_line(live("s_aaaaaaaaaaaaaaaa", 0));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", 0));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    insta::assert_snapshot!("quit_over_a_session", screen(&app, 160, 48));
}

#[test]
fn quit_on_home() {
    let mut app = home(80, 24);
    ask_quit(&mut app);
    insta::assert_snapshot!("quit_on_home", screen(&app, 80, 24));
}

#[test]
fn the_count_line_names_working_and_elsewhere() {
    let mut app = home(80, 24);
    app.on_line(hello());
    // Three working: the first also open elsewhere twice over, the
    // second once, the third nowhere.
    app.on_line(live("s_aaaaaaaaaaaaaaaa", 2));
    app.on_line(live("s_bbbbbbbbbbbbbbbb", 1));
    app.on_line(live("s_cccccccccccccccc", 0));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.quit_question(), Some((3, 2)));
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    let shown = crate::view::text(&buf);
    assert!(
        shown.contains("3 sessions working, 2 also open elsewhere"),
        "{shown}"
    );
}

#[test]
fn the_bar_stays_on_enter_and_arrows_do_nothing() {
    let mut app = home(80, 24);
    ask_quit(&mut app);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Up, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, clock.now()), Effect::None);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    // Exactly one barred row: Enter's.
    let barred = (0..24)
        .filter(|y| (0..80).any(|x| buf[(x, *y)].bg == crate::theme::Role::Accent.color()))
        .count();
    assert_eq!(barred, 1);
    let row = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .find(|row| row.contains("› Enter"))
        .expect("Enter's row keeps the bar");
    assert!(row.contains("leave them running"), "{row}");
}

#[test]
fn a_click_on_a_choice_does_what_its_key_does() {
    for (choice, key) in [
        (QuitChoice::Leave, Key::Enter),
        (QuitChoice::CloseAll, Key::Char('c')),
    ] {
        let mut app = home(80, 24);
        ask_quit(&mut app);
        let clock = fakes::clock::FakeClock::new();
        let by_key = app.on_key(key, clock.now());
        let mut app = home(80, 24);
        ask_quit(&mut app);
        let by_click = app.on_click(TargetId::Home(Spot::Quit(choice)));
        // Command ids mint at random: the effects agree past them.
        assert_eq!(stripped(&by_key), stripped(&by_click), "{choice:?}");
    }
    // Stay closes the question, on home or over a session.
    let mut app = home(80, 24);
    ask_quit(&mut app);
    assert_eq!(
        app.on_click(TargetId::Home(Spot::Quit(QuitChoice::Stay))),
        Effect::None
    );
    assert!(!app.quit_open());
    let mut app = home(80, 24);
    ask_quit(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert_eq!(
        app.on_click(TargetId::Home(Spot::Quit(QuitChoice::Stay))),
        Effect::None
    );
    assert!(!app.quit_open());
    // Past the question a choice click does nothing.
    assert_eq!(
        app.on_click(TargetId::Home(Spot::Quit(QuitChoice::Leave))),
        Effect::None
    );
}

/// An `Effect` with its minted command ids stripped: the commands,
/// sessions and args agree.
fn stripped(effect: &Effect) -> Effect {
    match effect {
        Effect::Exit(lines) => Effect::Exit(
            lines
                .iter()
                .map(|line| {
                    let mut line: serde_json::Value =
                        serde_json::from_str(line).unwrap_or_default();
                    if let Some(object) = line.as_object_mut() {
                        object.remove("id");
                    }
                    line.to_string()
                })
                .collect(),
        ),
        Effect::None => Effect::None,
        Effect::Quit => Effect::Quit,
        Effect::Send(_)
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_)
        | Effect::FindPause { .. }
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::OpenFile(_) => panic!("unexpected effect: {effect:?}"),
    }
}

#[test]
fn the_title_is_bold_accent_and_the_footer_dim() {
    let mut app = home(80, 24);
    ask_quit(&mut app);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    draw(&app, area, &mut buf, &mut Vec::new());
    let rows: Vec<String> = (0..24)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol().to_owned()).collect())
        .collect();
    let (title_y, _) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains("Quit"))
        .expect("the title row");
    let title_y = u16::try_from(title_y).unwrap_or(u16::MAX);
    let title_x = (0..80)
        .find(|x| buf[(*x, title_y)].symbol() == "Q")
        .expect("the title text");
    let cell = &buf[(title_x, title_y)];
    assert_eq!(cell.fg, crate::theme::Role::Accent.color());
    assert!(cell.modifier.contains(Modifier::BOLD));
    let foot = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.contains("click a choice"))
        .expect("the footer row");
    let y = u16::try_from(foot.0).unwrap_or(u16::MAX);
    assert!(
        (0..80).all(|x| !buf[(x, y)].modifier.contains(Modifier::BOLD)),
        "the footer stays dim"
    );
    assert!(
        (0..80)
            .filter(|x| buf[(*x, y)].symbol() != " ")
            .all(|x| buf[(x, y)].modifier.contains(Modifier::DIM)),
        "the footer stays dim"
    );
}

#[test]
fn quit_draws_over_the_model_picker_on_home() {
    let mut app = home(80, 24);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlL, clock.now()), Effect::None);
    assert!(app.model_picker_open());
    ask_quit(&mut app);
    assert!(app.quit_open());
    assert!(app.model_picker_open());
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("Quit"), "{shown}");
    assert!(shown.contains("leave them running"), "{shown}");
}

#[test]
fn quit_draws_over_the_config_view_on_home() {
    let mut app = home(80, 24);
    app.open_config_view(crate::app::ConfigView::Settings);
    assert!(app.config_view_open());
    ask_quit(&mut app);
    assert!(app.quit_open());
    assert!(app.config_view_open());
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("Quit"), "{shown}");
    assert!(shown.contains("leave them running"), "{shown}");
}

#[test]
fn quit_draws_over_keys_on_home() {
    let mut app = home(80, 24);
    let clock = fakes::clock::FakeClock::new();
    for ch in "/keys".chars() {
        assert_eq!(app.on_key(Key::Char(ch), clock.now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert!(app.keys_screen_open());
    ask_quit(&mut app);
    assert!(app.quit_open());
    assert!(app.keys_screen_open());
    let shown = screen(&app, 80, 24);
    assert!(shown.contains("Quit"), "{shown}");
    assert!(shown.contains("leave them running"), "{shown}");
}

#[test]
fn the_hint_row_no_longer_shows_the_question() {
    let mut app = home(80, 24);
    ask_quit(&mut app);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert!(!app.hint());
    let shown = screen(&app, 80, 24);
    // Once, in the overlay: never in the hint row.
    assert_eq!(shown.matches("sessions working").count(), 1, "{shown}");
}
