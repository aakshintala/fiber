//! Tests for `/logout` on the app: the two ticket screen tests (a
//! stored label deletes through confirm, a configured label lists dim
//! and deletes nothing), the click path over a dim row, and the view's
//! edges (`docs/tui.md`, "Logging out").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::{App, Effect};
use crate::Configure;
use crate::configure::{ConfigureError, LoginKind, LogoutRow};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::Key;
use crate::swapped::Spot;

/// The injected clock's now.
fn now() -> Instant {
    contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref())
}

/// A seam answering `rows` for the list.
fn logout_seam(rows: Vec<LogoutRow>) -> Arc<Fake> {
    let fake = Fake::new(Vec::new());
    if let Ok(mut logout) = fake.logout.lock() {
        *logout = Ok(rows);
    }
    Arc::new(fake)
}

/// A deletable stored label row.
fn stored(name: &str, label: &str, default: bool, siblings: &[&str]) -> LogoutRow {
    LogoutRow {
        name: name.to_owned(),
        label: Some(label.to_owned()),
        kind: LoginKind::Key,
        default,
        deletable: true,
        origin: None,
        siblings: siblings
            .iter()
            .map(|sibling| (*sibling).to_owned())
            .collect(),
    }
}

/// A label whose key comes from elsewhere: no delete can take it.
fn dim(name: &str, label: &str, origin: &str) -> LogoutRow {
    LogoutRow {
        name: name.to_owned(),
        label: Some(label.to_owned()),
        kind: LoginKind::Key,
        default: false,
        deletable: false,
        origin: Some(origin.to_owned()),
        siblings: Vec::new(),
    }
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

/// Opens `/logout` on `app`.
fn open_logout(app: &mut App) {
    app.open_config_view(super::ConfigView::Logout);
    assert!(app.config_view_open());
}

/// Stores `rel` under the fake home with plain `std::fs`, as a stored
/// credential file.
fn store_file(seam: &Fake, rel: &str) {
    let path = seam.home().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, "sk-live").unwrap();
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
fn logout_deletes_the_confirmed_label_and_drops_its_row() {
    let seam = logout_seam(vec![
        stored("acme", "work", true, &[]),
        stored("acme", "home", false, &[]),
    ]);
    if let Ok(mut deleted) = seam.deleted.lock() {
        *deleted = Ok("credentials/acme/work".to_owned());
    }
    store_file(&seam, "credentials/acme/work");
    store_file(&seam, "credentials/acme/home");
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // The list names the provider and the label, marking the default;
    // focus opens on the first deletable row.
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Log out") && row.contains("✕")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("acme")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("work")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("(default)")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("home")), "{rows:?}");

    // Enter asks for confirmation, deleting nothing yet.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());
    assert!(seam.home().join("credentials/acme/work").exists());
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/work?")),
        "{rows:?}"
    );

    // Confirm deletes the file and drops its row; the overlay stays
    // open on the remaining label with the removed line below it.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(
        deletes.unwrap_or_default(),
        [("acme".to_owned(), Some("work".to_owned()))]
    );
    assert!(!seam.home().join("credentials/acme/work").exists());
    assert!(seam.home().join("credentials/acme/home").exists());
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Removed credentials/acme/work.")),
        "{rows:?}"
    );

    // Focus moved to the remaining label: Enter confirms it next.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/home?")),
        "{rows:?}"
    );
}

#[test]
fn a_dim_origin_row_shows_its_source_and_enter_deletes_nothing() {
    let seam = logout_seam(vec![dim("acme", "prod", "environment variable ACME_KEY")]);
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("environment variable ACME_KEY")),
        "{rows:?}"
    );

    // Enter on a dim row does nothing: no confirm, no delete.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(
        !rows.iter().any(|row| row.contains("Delete acme/prod?")),
        "{rows:?}"
    );

    // Esc closes the view with nothing deleted.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());
}

#[test]
fn clicking_a_dim_row_neither_selects_nor_confirms_it() {
    let seam = logout_seam(vec![
        dim("acme", "prod", "environment variable ACME_KEY"),
        stored("acme", "work", true, &[]),
    ]);
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // Focus opens on the deletable row past the dim one. Clicking the
    // dim row's item does nothing: the selection stays where it was.
    assert_eq!(app.config_view_click(Spot::Row(1)), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());

    // Enter confirms the still-selected deletable row, not the dim one.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/work?")),
        "{rows:?}"
    );
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());
}

#[test]
fn moving_over_the_rows_reaches_a_dim_row_but_enter_does_nothing() {
    let seam = logout_seam(vec![
        stored("acme", "a", true, &[]),
        dim("acme", "b", "environment variable ACME_KEY"),
        stored("acme", "c", false, &[]),
    ]);
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // Down reaches the dim row; Enter there does nothing.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(deletes.unwrap_or_default(), Vec::new());
    let rows = screen(&app);
    assert!(
        !rows.iter().any(|row| row.contains("Delete acme/b?")),
        "{rows:?}"
    );

    // Down reaches the last label; Enter confirms it.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/c?")),
        "{rows:?}"
    );

    // Esc goes back to the list, still open on the same rows.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.contains("acme")), "{rows:?}");
}

#[test]
fn an_empty_list_shows_the_note_and_enter_does_nothing() {
    let seam = logout_seam(Vec::new());
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row
            .contains("No provider is installed, and no installed extension declares a secret.")),
        "{rows:?}"
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
}

#[test]
fn a_failed_delete_keeps_the_row_and_says_why() {
    let seam = logout_seam(vec![stored("acme", "work", true, &[])]);
    if let Ok(mut deleted) = seam.deleted.lock() {
        *deleted = Err(ConfigureError {
            code: contract::ErrorCode::IoFailed,
            message: "another login or logout holds its lock".to_owned(),
        });
    }
    store_file(&seam, "credentials/acme/work");
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(
        deletes.unwrap_or_default(),
        [("acme".to_owned(), Some("work".to_owned()))]
    );
    // The failed delete removed nothing: the row and the file stay,
    // with the seam's message below the list.
    assert!(seam.home().join("credentials/acme/work").exists());
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.contains("acme")), "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("another login or logout holds its lock")),
        "{rows:?}"
    );
}

#[test]
fn a_relist_after_delete_names_the_error_beside_the_removed_line() {
    let seam = logout_seam(vec![stored("acme", "work", true, &[])]);
    if let Ok(mut deleted) = seam.deleted.lock() {
        *deleted = Ok("credentials/acme/work".to_owned());
    }
    store_file(&seam, "credentials/acme/work");
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // The delete lands but the re-list fails: both lines stay, and the
    // row is gone.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    if let Ok(mut logout) = seam.logout.lock() {
        *logout = Err(ConfigureError {
            code: contract::ErrorCode::IoFailed,
            message: "the hub is gone".to_owned(),
        });
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(!seam.home().join("credentials/acme/work").exists());
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Removed credentials/acme/work.")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("the hub is gone")),
        "{rows:?}"
    );
}

#[test]
fn the_confirm_names_siblings_and_the_removed_line_keeps_them() {
    let seam = logout_seam(vec![stored("acme", "work", true, &["beta"])]);
    if let Ok(mut deleted) = seam.deleted.lock() {
        *deleted = Ok("credentials/acme/work".to_owned());
    }
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // The confirm names who else reads the stored directory.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Also used by: beta")),
        "{rows:?}"
    );

    // The removed line keeps them, mirroring the CLI's removal message.
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("Removed credentials/acme/work, which beta also reads.")),
        "{rows:?}"
    );
}

#[test]
fn a_secret_row_confirms_by_name_and_deletes_with_no_label() {
    let seam = logout_seam(vec![LogoutRow {
        name: "acme.api_key".to_owned(),
        label: None,
        kind: LoginKind::Secret,
        default: false,
        deletable: true,
        origin: None,
        siblings: Vec::new(),
    }]);
    if let Ok(mut deleted) = seam.deleted.lock() {
        *deleted = Ok("credentials/acme.api_key".to_owned());
    }
    store_file(&seam, "credentials/acme.api_key");
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("acme.api_key")),
        "{rows:?}"
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme.api_key?")),
        "{rows:?}"
    );
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let deletes = seam.deletes.lock().map(|deletes| deletes.clone());
    assert_eq!(
        deletes.unwrap_or_default(),
        [("acme.api_key".to_owned(), None)]
    );
    assert!(!seam.home().join("credentials/acme.api_key").exists());
}
