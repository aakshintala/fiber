//! Tests for `/logout` on the app: the two ticket screen tests (a
//! stored label deletes through confirm, a configured label lists dim
//! and deletes nothing), the click path over a dim row, and the view's
//! edges (`docs/tui.md`, "Logging out").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::super::{App, Effect};
use crate::Configure;
use crate::configure::{ConfigureError, LogoutRow};
use crate::configure_fake::Fake;
use crate::home::Launch;
use crate::keys::{Key, Mouse, MouseKind};
use crate::swapped::Spot;

/// The injected clock's now.
fn now() -> Instant {
    contract::clock::Clock::now(fakes::clock::FakeClock::new().as_ref())
}

/// Thirty labels: more than the sixteen body rows an 80x24 screen
/// shows, so the list scrolls.
fn many_labels() -> Vec<LogoutRow> {
    (0..30)
        .map(|at| stored("acme", &format!("l{at:02}"), at == 0, &[]))
        .collect()
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
    LogoutRow::Provider {
        name: name.to_owned(),
        label: label.to_owned(),
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
    LogoutRow::Provider {
        name: name.to_owned(),
        label: label.to_owned(),
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

/// The screen's buffer, for style assertions text cannot see.
fn rendered(app: &App) -> Buffer {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    buf
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

    // The focus highlight sits on the first label's row: the `› ` gutter
    // names the focused choice, and the other label carries none.
    assert!(
        rows.iter()
            .any(|row| row.contains('›') && row.contains("work")),
        "{rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains('›') && row.contains("home")),
        "{rows:?}"
    );

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

    // The dim row draws dimmed while the deletable row draws full:
    // without the negation the live row dims and the dim row shines.
    let buf = rendered(&app);
    let text_at = |y: u16| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>();
    let dim_y = (0..24)
        .find(|y| text_at(*y).contains("environment variable ACME_KEY"))
        .expect("no dim row");
    assert!(
        (0..80).any(|x| buf[(x, dim_y)].modifier.contains(Modifier::DIM)),
        "the dim row draws with no dimmed cell"
    );
    let live_y = (0..24)
        .find(|y| text_at(*y).contains("acme work"))
        .expect("no live row");
    assert!(
        (0..80).all(|x| !buf[(x, live_y)].modifier.contains(Modifier::DIM)),
        "the deletable row draws dimmed"
    );

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
    // A blank row separates the list body from the said line: without
    // the separator the message runs directly against the last row.
    let said_at = rows
        .iter()
        .position(|row| row.contains("another login or logout holds its lock"))
        .expect("no said line");
    assert!(said_at > 0, "{rows:?}");
    assert!(rows[said_at - 1].trim().is_empty(), "{rows:?}");
}

#[test]
fn a_relist_after_delete_names_the_error_beside_the_removed_line() {
    let seam = logout_seam(vec![
        stored("acme", "work", true, &[]),
        stored("acme", "home", false, &[]),
    ]);
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
    // The failed re-list drops exactly the deleted row: its row is
    // gone while its same-provider sibling survives.
    assert!(rows.iter().any(|row| row.contains("home")), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("acme work")),
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
    let seam = logout_seam(vec![LogoutRow::Secret {
        name: "acme.api_key".to_owned(),
        deletable: true,
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

#[test]
fn up_moves_back_over_the_target_rows() {
    let seam = logout_seam(vec![
        stored("acme", "aaa", true, &[]),
        stored("acme", "bbb", false, &[]),
        stored("acme", "ccc", false, &[]),
    ]);
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // Down twice reaches the last label; two Ups return to the first,
    // and Enter confirms it.
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/aaa?")),
        "{rows:?}"
    );
}

#[test]
fn paging_moves_a_page_of_labels_each_step() {
    let seam = logout_seam(many_labels());
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // A page is the shown body rows less one: PageDown from the first
    // label lands twelve rows below, and Enter confirms that label.
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/l12?")),
        "{rows:?}"
    );

    // Esc returns to the list; PageUp lands back on the first label.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/l00?")),
        "{rows:?}"
    );
}

#[test]
fn providers_and_secrets_list_under_their_own_headings() {
    let seam = logout_seam(vec![
        stored("acme", "work", true, &[]),
        LogoutRow::Secret {
            name: "acme.api_key".to_owned(),
            deletable: true,
        },
    ]);
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // Each row lists under its own heading, with a blank row between
    // the groups.
    let rows = screen(&app);
    let providers = rows
        .iter()
        .position(|row| row.contains("Providers"))
        .expect("no Providers heading");
    let work = rows
        .iter()
        .position(|row| row.contains("acme work"))
        .expect("no provider row");
    let secrets = rows
        .iter()
        .position(|row| row.contains("Secrets"))
        .expect("no Secrets heading");
    let secret = rows
        .iter()
        .position(|row| row.contains("acme.api_key"))
        .expect("no secret row");
    assert!(
        providers < work && work < secrets && secrets < secret,
        "{rows:?}"
    );
    assert!(rows[work + 1].trim().is_empty(), "{rows:?}");
}

#[test]
fn a_tall_list_scrolls_its_window_with_focus() {
    let seam = logout_seam(many_labels());
    let mut app = attached(Some(Arc::clone(&seam)));
    open_logout(&mut app);

    // Thirteen body rows show over the view: twelve Downs put focus on
    // the first clipped row, scrolling the window by one.
    for _ in 0..12 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let rows = screen(&app);
    assert!(
        !rows.iter().any(|row| row.contains("Providers")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("acme l00")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme l12")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("acme l13")), "{rows:?}");

    // Two rows further on the top rows stay clipped.
    for _ in 0..2 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let rows = screen(&app);
    assert!(
        !rows.iter().any(|row| row.contains("Providers")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("acme l00")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("acme l01")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme l02")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme l14")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("acme l15")), "{rows:?}");

    // On the last row the first labels are clipped away.
    for _ in 0..15 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let rows = screen(&app);
    assert!(
        !rows.iter().any(|row| row.contains("Providers")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("acme l00")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("acme l16")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme l17")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("acme l29")), "{rows:?}");
}

/// A wheel report at (`col`, `row`): down steps toward the tail, up away
/// from it, over the conversation column the logout view takes.
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

/// The wheel over the logout view's delete confirmation is swallowed: a
/// step that does not scroll takes no wheel, and the hidden conversation
/// keeps its scroll (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_the_delete_confirmation_is_swallowed() {
    let seam = logout_seam(vec![stored("acme", "work", true, &[])]);
    store_file(&seam, "credentials/acme/work");
    let mut app = attached(Some(Arc::clone(&seam)));
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_logout(&mut app);
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let rows = screen(&app);
    assert!(
        rows.iter().any(|row| row.contains("Delete acme/work?")),
        "{rows:?}"
    );
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    assert_eq!(app.on_wheel(&wheel(10, 5, true)), Effect::None);
    assert_eq!(app.on_wheel(&wheel(10, 5, false)), Effect::None);
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(app.config_view_open(), "the confirmation stays open");
}
