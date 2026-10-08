//! Tests for the look on the screen: the frame written is painted, the
//! frame kept is not, and `run` shows a theme file's notice.

use std::io::Write;
use std::sync::mpsc;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::tests::{DEADLINE, launch, open, read_until};
use crate::app::App;
use crate::look::{Look, ThemeSetting};
use crate::mouse::Target;
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
    assert_eq!(written[(0, 0)].fg, Color::Rgb(86, 182, 194));
    assert_eq!(written[(1, 0)].fg, Color::Rgb(0xdc, 0xdf, 0xe4));
    assert_eq!(written[(3, 1)].bg, Color::Rgb(0x1e, 0x21, 0x27));
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
    read_until(&pair.main, b"\"solar\":", "the theme notice");
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The rest of what it writes is read, so no write blocks on a full pty.
    read_until(&pair.main, b"\x1b[?1049l", "the restore bytes");
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}
