//! Tests for `/login` on the app: opening it from the slash list and
//! from a failed turn's "log in" line, the keys it takes and the ones it
//! leaves, and that a typed key never reaches the draft or a `Debug`
//! (`docs/tui.md`, "Logging in", "Turns", "Slash commands").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::{App, Effect, Target};
use crate::Configure;
use crate::configure::{LoginKind, LoginTarget};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::mouse::TargetId;

/// The injected clock's now.
fn now() -> Instant {
    contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref())
}

/// A seam with one key provider.
fn seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "acme".to_owned(),
            kind: LoginKind::Key,
        }]);
    }
    Arc::new(fake)
}

/// An app on home at 80x24 in `/w`, with `seam`.
fn home(seam: Option<Arc<Fake>>) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        ..Default::default()
    });
    app.set_configure(seam.map(|seam| seam as Arc<dyn Configure>));
    app.set_size(80, 24);
    app
}

/// An app attached to a session, with `seam`.
fn attached(seam: Option<Arc<Fake>>) -> App {
    let mut app = home(seam);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// Types `/login` and presses Enter.
fn slash_login(app: &mut App) -> Effect {
    for ch in "/login".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

/// The screen's rows as text.
fn screen(app: &App) -> Vec<String> {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn slash_login_opens_the_view_attached_and_on_home() {
    let mut app = home(Some(seam()));
    assert_eq!(slash_login(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
    let rows = screen(&app);
    assert!(
        rows.first().is_some_and(|row| row.starts_with("Log in")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("acme  key")), "{rows:?}");

    let mut app = attached(Some(seam()));
    slash_login(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.starts_with("Log in")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme  key")), "{rows:?}");
}

#[test]
fn without_a_seam_login_says_not_available() {
    let mut app = attached(None);
    slash_login(&mut app);
    let frame = app.config_view_screen();
    assert_eq!(
        frame.as_ref().map(|frame| frame.title.as_str()),
        Some("Log in")
    );
    assert_eq!(
        frame.map(|frame| frame.below),
        Some(vec!["Not available in this terminal.".to_owned()])
    );
    app.on_key(Key::Esc, now());
    assert!(!app.config_view_open());
}

#[test]
fn a_paste_reaches_the_key_field_not_the_draft() {
    let seam = seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_login(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    // Twelve lines, two past the ten where a draft would tokenize.
    let pasted: String = (1..=12)
        .map(|n| format!("a{n}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    app.on_edit(Edit::Paste(pasted.clone()));
    assert!(app.draft().is_empty());
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(seam.secret_of(0), pasted.trim());
    assert_eq!(seam.stores(), [("acme".to_owned(), None)]);
}

#[test]
fn ctrl_c_still_reaches_the_quit_flow_from_the_key_panel() {
    let mut app = attached(Some(seam()));
    slash_login(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    assert!(app.config_view_open());
    let at = now();
    assert_eq!(app.on_key(Key::CtrlC, at), Effect::None);
    let later = at.checked_add(Duration::from_millis(10)).unwrap_or(at);
    assert_eq!(app.on_key(Key::CtrlC, later), Effect::Quit);
}

#[test]
fn the_failed_turns_log_in_line_opens_the_view() {
    let mut app = attached(Some(seam()));
    assert_eq!(app.on_click(TargetId::Line(Target::Login)), Effect::None);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.starts_with("Log in")), "{rows:?}");

    // Enter on the focused line reaches the same arm.
    let mut app = attached(Some(seam()));
    app.focus = Some(TargetId::Line(Target::Login));
    assert!(app.focus_key(&Key::Enter).is_some());
    assert!(app.config_view_open());
}

#[test]
fn the_key_never_appears_in_the_apps_debug() {
    let mut app = attached(Some(seam()));
    slash_login(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    for ch in "sk-SECRET-1".chars() {
        app.on_key(Key::Char(ch), now());
    }
    let debug = format!("{:?}", app.config_views.open);
    assert!(!debug.contains("SECRET"), "{debug}");
    assert!(!debug.contains("sk-"), "{debug}");
}
