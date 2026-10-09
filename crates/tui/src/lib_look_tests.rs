//! Tests for the look on the screen: the frame written is painted, the
//! frame kept is not, and `run` shows a theme file's notice.

use std::io::Write;
use std::sync::mpsc;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::tests::{DEADLINE, launch, open};
use crate::app::App;
use crate::look::{Look, ThemeSetting};
use crate::mouse::Target;
use crate::pty_watch::{watch, watched};
use crate::screen::Screen;
use crate::theme::Role;

/// The look for `setting` on a terminal with `vars` set.
fn look(setting: ThemeSetting, vars: &[(&str, &str)]) -> Look {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let (look, notice) = Look::new(setting, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    assert_eq!(notice, None);
    look
}

/// Draws "hi" at the top left, its first cell in the accent colour.
fn hi(_: &App, _: Rect, buf: &mut Buffer, _: Option<(u16, u16)>) -> Vec<Target> {
    buf.set_string(0, 0, "hi", ratatui::style::Style::new());
    if let Some(cell) = buf.cell_mut((0, 0)) {
        cell.fg = Role::Accent.color();
    }
    Vec::new()
}

/// A 4x2 screen painted with `look`, after one draw of [`hi`].
fn drawn(look: Look) -> Screen<TestBackend> {
    let mut screen = Screen::new(TestBackend::new(4, 2), 4, 2).expect("a screen");
    screen.set_look(look);
    let mut app = App::new(std::path::PathBuf::from("/w"));
    screen.draw_with(&mut app, None, hi).expect("a draw");
    screen
}

#[test]
fn the_frame_written_is_painted_and_the_kept_frame_is_not() {
    let screen = drawn(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    let written = screen.backend().buffer();
    assert_eq!(written[(0, 0)].fg, Color::Rgb(0x6e, 0xaa, 0xfe));
    assert_eq!(written[(1, 0)].fg, Color::Reset);
    assert_eq!(written[(3, 1)].bg, Color::Reset);
    let (kept, _) = screen.last().expect("a kept frame");
    assert_eq!(kept[(0, 0)].fg, Role::Accent.color());
    assert_eq!(kept[(1, 0)].fg, Role::Text.color());
    assert_eq!(kept[(3, 1)].bg, Role::Background.color());
}

#[test]
fn a_fixed_light_theme_paints_every_cell_light() {
    let screen = drawn(look(ThemeSetting::Light, &[("COLORTERM", "truecolor")]));
    let written = screen.backend().buffer();
    for cell in &written.content {
        assert_eq!(cell.bg, Color::Rgb(0xfa, 0xfa, 0xfa));
    }
    assert_eq!(written[(1, 0)].fg, Color::Rgb(0x38, 0x3a, 0x42));
    assert_eq!(written[(2, 1)].fg, Color::Rgb(0x38, 0x3a, 0x42));
}

#[test]
fn no_color_leaves_every_cell_reset() {
    let screen = drawn(look(ThemeSetting::Dark, &[("NO_COLOR", "1")]));
    for cell in &screen.backend().buffer().content {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
    }
}

#[test]
fn a_new_look_repaints_the_unchanged_frame() {
    let mut screen = drawn(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    screen.set_look(look(ThemeSetting::Light, &[("COLORTERM", "truecolor")]));
    let mut app = App::new(std::path::PathBuf::from("/w"));
    screen.draw_with(&mut app, None, hi).expect("a draw");
    assert_eq!(
        screen.backend().buffer()[(3, 1)].bg,
        Color::Rgb(0xfa, 0xfa, 0xfa)
    );
}

#[test]
fn run_shows_a_theme_files_notice() {
    let mut pair = open();
    let frames = watch(
        &pair.main,
        vec![b"\"solar\":" as &[u8], b"\x1b[?1049l" as &[u8]],
    );
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-look-run".to_owned())
        .spawn(move || {
            let mut launch = launch();
            launch.theme = ThemeSetting::File {
                name: "solar".to_owned(),
                text: Err("gone".to_owned()),
            };
            let code = super::run(
                slave,
                launch,
                Box::new(|| Err(std::io::Error::other("refused"))),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Unchanged cells are skipped, spaces included, so one word is matched.
    watched(&frames, "the theme notice");
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The rest of what it writes is read, so no write blocks on a full pty.
    watched(&frames, "the restore bytes");
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}

/// A 4x2 screen painted with a following look, after one draw of [`hi`].
fn following() -> Screen<TestBackend> {
    drawn(look(ThemeSetting::Follow, &[("COLORTERM", "truecolor")]))
}

#[test]
fn a_new_look_keeps_the_reported_appearance() {
    use crate::look::Appearance;
    let mut screen = following();
    screen.appearance(Appearance::Light);
    // A picked theme still follows the light terminal: the carried report
    // re-resolves it before it is stored.
    screen.set_look(look(ThemeSetting::Follow, &[("COLORTERM", "truecolor")]));
    let mut app = App::new(std::path::PathBuf::from("/w"));
    screen.draw_with(&mut app, None, hi).expect("a draw");
    assert_eq!(
        screen.backend().buffer()[(3, 1)].bg,
        Color::Rgb(0xfa, 0xfa, 0xfa)
    );
}

#[test]
fn a_light_report_repaints_the_unchanged_frame_light() {
    use crate::look::Appearance;
    let mut screen = following();
    screen.appearance(Appearance::Light);
    // The report cleared the kept frame, so the same frame paints again,
    // light this time.
    let mut app = App::new(std::path::PathBuf::from("/w"));
    screen.draw_with(&mut app, None, hi).expect("a draw");
    assert_eq!(
        screen.backend().buffer()[(3, 1)].bg,
        Color::Rgb(0xfa, 0xfa, 0xfa)
    );
}

#[test]
fn a_fixed_dark_theme_stays_dark_after_a_light_report() {
    use crate::look::Appearance;
    let mut screen = following();
    screen.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    screen.appearance(Appearance::Light);
    let mut app = App::new(std::path::PathBuf::from("/w"));
    screen.draw_with(&mut app, None, hi).expect("a draw");
    assert_eq!(screen.backend().buffer()[(3, 1)].bg, Color::Reset);
}

#[test]
fn a_light_report_through_the_loop_repaints_light() {
    use super::tests::new_loop;
    use crate::Input;
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (_, step_rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &step_rx), None);
    assert_eq!(lp.screen.backend().buffer()[(0, 0)].bg, Color::Reset);
    assert_eq!(
        lp.step(Input::Bytes(b"\x1b[?997;2n".to_vec()), &step_rx),
        None
    );
    assert_eq!(
        lp.screen.backend().buffer()[(0, 0)].bg,
        Color::Rgb(0xfa, 0xfa, 0xfa)
    );
}

#[test]
fn the_rail_and_panel_regions_paint_as_the_background() {
    // The rail's and the panel's regions paint as the theme's
    // background, not the surface tint (`docs/tui.md`, "Themes").
    use crate::home::Launch;
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: std::path::PathBuf::from("/w"),
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
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.set_size(160, 40);
    // Two live sessions, so the rail draws.
    for session in ["s_aaaaaaaaaaaaaaaa", "s_bbbbbbbbbbbbbbbb"] {
        app.on_line(crate::link::Line::Session(contract::Envelope {
            kind: "session_status".to_owned(),
            session_id: contract::SessionId(session.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::json!({
                "name": "fix the parser", "workspace": "/w",
                "project": "-w", "state": "idle", "since": 0,
                "spend": {"tokens": {"input": 1, "cache_read": 0,
                    "cache_write": {}, "output": 2},
                    "cost": 0.0, "subscription_cost": 0.0},
                "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        }));
    }
    let layout = app.chrome().layout().expect("a layout");
    let panel = layout.panel.expect("a panel");
    let rail = layout.rail.expect("a rail");
    let mut screen = Screen::new(TestBackend::new(160, 40), 160, 40).expect("a screen");
    screen.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    screen
        .draw_with(&mut app, None, crate::view::render)
        .expect("a draw");
    let buf = screen.backend().buffer();
    // Dark `background` against dark `surface` (`docs/tui.md`, "Themes").
    let background = Color::Reset;
    assert_eq!(
        buf[(panel.right().saturating_sub(1), panel.y)].bg,
        background
    );
    assert_eq!(
        buf[(rail.x, rail.bottom().saturating_sub(1))].bg,
        background
    );
}
