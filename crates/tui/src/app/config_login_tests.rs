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
use crate::keys::{Edit, Key, Mouse, MouseKind};
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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        rows.iter()
            .any(|row| row.contains("Log in") && row.contains("✕")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("▄")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme  key")), "{rows:?}");

    let mut app = attached(Some(seam()));
    slash_login(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Log in") && row.contains("✕")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("▄")), "{rows:?}");
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
    assert!(
        rows.iter()
            .any(|row| row.contains("Log in") && row.contains("✕")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("▄")), "{rows:?}");

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

/// A seam with one browser provider.
fn browser_seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "codex".to_owned(),
            kind: LoginKind::Browser,
        }]);
    }
    Arc::new(fake)
}

/// An app with the login view open on the browser row, selected.
fn browser_login_app(seam: &Arc<Fake>) -> App {
    let mut app = attached(Some(Arc::clone(seam)));
    slash_login(&mut app);
    assert!(app.config_view_open());
    app.on_key(Key::Down, now());
    app
}

/// Presses Enter on the browser row and takes the login start.
fn start_login(app: &mut App) -> crate::login_worker::LoginStart {
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    app.take_login_start()
        .expect("Enter on a browser row queues a login start")
}

#[test]
fn enter_on_a_browser_row_queues_a_ticketed_start() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    assert_eq!(start.ticket, crate::login_worker::LoginTicket(1));
    assert_eq!(start.name, "codex");
    assert!(app.take_login_start().is_none());
    // Esc returns to the rows; a second start waits on ticket 2.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    app.on_key(Key::Down, now());
    let start = start_login(&mut app);
    assert_eq!(start.ticket, crate::login_worker::LoginTicket(2));
    assert_eq!(start.name, "codex");
}

#[test]
fn only_the_waiting_ticket_s_open_returns_its_url() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let url = "https://auth.example/authorize?state=1".to_owned();
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Open(url.clone())
        ),
        Some(url)
    );
    assert_eq!(
        app.on_login(
            start.ticket.next(),
            crate::login_worker::LoginStep::Open("https://auth.example/other".to_owned())
        ),
        None
    );
}

#[test]
fn y_copies_the_waiting_url_whole() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    // Before the URL arrives `y` does nothing.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    let start = start_login(&mut app);
    // `y` before the URL arrives does nothing.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    let url = "https://auth.example/authorize?state=1".to_owned();
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Open(url.clone())
        ),
        Some(url.clone())
    );
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::Copy(url));
    assert!(app.copied);
}

/// Starts `start`'s worker as the loop would and keeps it in the view: the
/// run is answered at once, so no thread waits on the test.
fn keep_worker(
    app: &mut App,
    seam: &Arc<Fake>,
    start: crate::login_worker::LoginStart,
) -> Arc<crate::configure_fake::FakeLogin> {
    let (tx, _rx) = std::sync::mpsc::channel();
    let worker = crate::login_worker::start(start, fakes::clock::FakeClock::new(), tx);
    let login = seam.login(0).expect("one login started");
    seam.answer(
        0,
        Err(crate::configure::ConfigureError {
            code: contract::ErrorCode::AuthenticationFailed,
            message: "the login was cancelled; nothing was stored.".to_owned(),
        }),
    );
    app.login_started(worker);
    login
}

#[test]
fn esc_over_a_waiting_login_cancels_once() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let login = keep_worker(&mut app, &seam, start);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(login.cancels(), 1);
}

#[test]
fn a_started_worker_for_the_waiting_ticket_is_held_by_the_view() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let login = keep_worker(&mut app, &seam, start);
    assert_eq!(login.cancels(), 0);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(login.cancels(), 1);
}

#[test]
fn closing_a_waiting_login_cancels_once() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let login = keep_worker(&mut app, &seam, start);
    app.config_view_click(crate::swapped::Spot::Close);
    assert_eq!(login.cancels(), 1);
}

#[test]
fn opening_settings_over_a_waiting_login_cancels_once() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let login = keep_worker(&mut app, &seam, start);
    app.open_config_view(super::ConfigView::Settings);
    assert_eq!(login.cancels(), 1);
}

#[test]
fn a_stale_done_after_close_changes_nothing_and_opens_nothing() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    let ticket = start.ticket;
    let login = keep_worker(&mut app, &seam, start);
    app.config_view_click(crate::swapped::Spot::Close);
    assert_eq!(login.cancels(), 1);
    assert!(!app.config_view_open());
    assert_eq!(
        app.on_login(
            ticket,
            crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
}

#[test]
fn the_login_view_is_offered_in_every_mode() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    assert!(app.login_view().is_some());
    assert!(app.config_view_screen().is_none());
    // Waiting on a browser login keeps the view offered, drawn as the step.
    let _ = start_login(&mut app);
    assert!(app.login_view().is_some());
    assert!(app.config_view_screen().is_none());
    // Esc returns to the rows, which the view draws again.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.login_view().is_some());
    assert!(app.config_view_screen().is_none());
    // Esc closes the view, which offers nothing.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.login_view().is_none());
}

/// A seam with two key providers.
fn two_key_seam() -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![
            LoginTarget {
                name: "acme".to_owned(),
                kind: LoginKind::Key,
            },
            LoginTarget {
                name: "beta".to_owned(),
                kind: LoginKind::Key,
            },
        ]);
    }
    Arc::new(fake)
}

/// Reviewer choices with `models` and `selected` preselected.
fn choices(models: &[&str], selected: Option<&str>) -> crate::configure::ReviewerModels {
    crate::configure::ReviewerModels {
        models: models.iter().map(|model| (*model).to_owned()).collect(),
        selected: selected.map(str::to_owned),
        notices: Vec::new(),
    }
}

/// Opens `/login`, submits the key on the first row, and takes the
/// queued reviewer start.
fn start_key_reviewer(app: &mut App, seam: &Arc<Fake>) -> crate::login_worker::LoginStart {
    slash_login(app);
    assert!(app.config_view_open());
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    for ch in "sk-a".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(seam.stores(), [("acme".to_owned(), None)]);
    app.take_login_start()
        .expect("a stored key queues a reviewer read")
}

#[test]
fn a_stored_key_queues_a_reviewer_read_on_a_fresh_ticket() {
    let seam = two_key_seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    let start = start_key_reviewer(&mut app, &seam);
    assert_eq!(start.ticket, crate::login_worker::LoginTicket(1));
    assert_eq!(start.name, "acme");
    assert!(matches!(start.job, crate::login_worker::LoginJob::Reviewer));
    assert!(app.take_login_start().is_none());
    // The preparing step shows while the read runs.
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Reading acme's reviewer models")),
        "{rows:?}"
    );
}

#[test]
fn a_declared_secret_ends_without_a_reviewer_read() {
    let fake = Fake::new(Vec::new());
    if let Ok(mut targets) = fake.targets.lock() {
        *targets = Ok(vec![LoginTarget {
            name: "acme.api_key".to_owned(),
            kind: crate::configure::LoginKind::Secret,
        }]);
    }
    let seam = Arc::new(fake);
    let mut app = attached(Some(Arc::clone(&seam)));
    slash_login(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    for ch in "v1".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.take_login_start().is_none());
    assert!(seam.reviewer_reads().is_empty());
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("✓ Logged in to acme.api_key.")),
        "{rows:?}"
    );
}

#[test]
fn a_browser_done_keeps_its_ticket_into_preparation() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
    // Done success is not the end: the ticket prepares, and a repeated
    // Done cannot re-enter discovery after completion.
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Reading codex's reviewer models")),
        "{rows:?}"
    );
}

#[test]
fn a_late_reviewer_after_dismissal_changes_nothing_and_saves_nothing() {
    let seam = two_key_seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    let start = start_key_reviewer(&mut app, &seam);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert!(app.config_view_open());
    assert!(seam.reviewer_saves().is_empty());
}

#[test]
fn a_second_login_leaves_the_first_ticket_s_result_behind() {
    let seam = two_key_seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    let first = start_key_reviewer(&mut app, &seam);
    // Back to the rows, over to beta, and through its panel: the
    // second read runs on ticket 2.
    let _ = app.on_key(Key::Esc, now());
    app.on_key(Key::Down, now());
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    for ch in "sk-b".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        seam.stores(),
        [("acme".to_owned(), None), ("beta".to_owned(), None)]
    );
    let second = app.take_login_start().expect("a second reviewer read");
    assert_eq!(second.ticket, crate::login_worker::LoginTicket(2));
    assert_eq!(second.name, "beta");
    // The first ticket's late choices land on no preparing view.
    assert_eq!(
        app.on_login(
            first.ticket,
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert!(seam.reviewer_saves().is_empty());
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Reading beta's reviewer models")),
        "{rows:?}"
    );
}

#[test]
fn ticket_exhaustion_hands_max_out_again_and_admits_its_old_result() {
    let seam = two_key_seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    app.config_views.tickets = crate::login_worker::LoginTicket(u64::MAX - 1);
    let first = start_key_reviewer(&mut app, &seam);
    assert_eq!(first.ticket, crate::login_worker::LoginTicket(u64::MAX));
    let _ = app.on_key(Key::Esc, now());
    app.on_key(Key::Down, now());
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    for ch in "sk-b".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let second = app.take_login_start().expect("a second reviewer read");
    assert_eq!(second.ticket, crate::login_worker::LoginTicket(u64::MAX));
    // The old read's late result passes the ticket/stage guard onto the
    // later login waiting on the same saturated ticket: this documents
    // the reuse, without saving a choice.
    assert_eq!(
        app.on_login(
            first.ticket,
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["old-m"], Some("old-m"))))
        ),
        None
    );
    assert!(seam.reviewer_saves().is_empty());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.contains("old-m")), "{rows:?}");
}

#[test]
fn closing_after_the_credential_commit_keeps_it() {
    let seam = browser_seam();
    let mut app = browser_login_app(&seam);
    let start = start_login(&mut app);
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Done(Ok(crate::configure::Stored {
                path: "credentials/codex/alice@example.com".to_owned(),
                replaced: false,
            }))
        ),
        None
    );
    app.config_view_click(crate::swapped::Spot::Close);
    assert!(!app.config_view_open());
}

#[test]
fn the_key_never_reaches_the_apps_debug_through_the_reviewer_flow() {
    let seam = two_key_seam();
    let mut app = attached(Some(Arc::clone(&seam)));
    let start = start_key_reviewer(&mut app, &seam);
    assert_eq!(
        app.on_login(
            start.ticket,
            crate::login_worker::LoginStep::Reviewer(Ok(choices(&["m1"], Some("m1"))))
        ),
        None
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(
        seam.reviewer_saves(),
        [("acme".to_owned(), "m1".to_owned())]
    );
    let debug = format!("{:?}", app.login_view());
    assert!(!debug.contains("sk-a"), "{debug}");
}

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the login view takes.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// The wheel over the login view's open key panel is swallowed: a step
/// that does not scroll takes no wheel, and the hidden conversation
/// keeps its scroll (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_the_open_key_panel_is_swallowed() {
    let mut app = attached(Some(seam()));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    slash_login(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Enter, now());
    assert!(app.config_view_open());
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    assert_eq!(app.on_wheel(&wheel(10, 5, true)), Effect::None);
    assert_eq!(app.on_wheel(&wheel(10, 5, false)), Effect::None);
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(app.config_view_open(), "the panel stays open");
}
