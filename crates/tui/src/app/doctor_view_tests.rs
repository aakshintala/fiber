//! Tests for the `/doctor` view on the app: opening the report on home
//! and attached, whole-line clipping, scrolling, and the remote-home
//! route that never reaches the hub (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::{App, Effect};
use super::ConfigView;
use crate::Configure;
use crate::app::testkit::{FailingHub, SESSION, now};
use crate::configure::{Shown, WriteScope};
use crate::configure_fake::{Fake, row};
use crate::home::Launch;
use crate::keys::{Edit, Key};
use crate::mouse::TargetId;
use crate::swapped::Spot;

/// A seam over two rows.
fn fake() -> Arc<Fake> {
    let mut fake = Fake::new(vec![
        row(
            "handoff.tokens",
            Shown::Value("400000".to_owned()),
            "default",
            WriteScope::Any { repo: true },
        ),
        row(
            "tui.theme",
            Shown::Unset,
            "default",
            WriteScope::Any { repo: false },
        ),
    ]);
    fake.themes = vec!["solar".to_owned()];
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

/// A doctor report reader returning `report`.
fn doctor_reader(report: &str) -> contract::feedback::ReadDoctor {
    let report = report.to_owned();
    Arc::new(move || Ok(report.clone()))
}

/// An app with `seam` and a doctor reader returning `report`.
fn doctor_app(seam: Option<Arc<Fake>>, report: &str) -> App {
    let mut app = home(seam);
    app.set_doctor(Some(doctor_reader(report)));
    app
}

/// Types `/doctor` and presses Enter.
fn slash_doctor(app: &mut App) -> Effect {
    for ch in "/doctor".chars() {
        app.on_key(Key::Char(ch), now());
    }
    app.on_key(Key::Enter, now())
}

#[test]
fn slash_doctor_opens_the_report_view_on_home_and_attached() {
    let report = "fiber 0.0.1\nsecond line\n\nfourth";
    let mut app = doctor_app(Some(fake()), report);
    assert_eq!(slash_doctor(&mut app), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Doctor");
    assert_eq!(frame.command, "/doctor");
    assert!(frame.below.is_empty());
    assert!(frame.field.is_none());
    assert!(frame.footer.is_empty());
    // One plain row per snapshot line: the view renders the capture
    // verbatim, so it never differs from the headless door's output.
    let expected: Vec<String> = report.lines().map(str::to_owned).collect();
    assert_eq!(expected, ["fiber 0.0.1", "second line", "", "fourth"]);
    assert_eq!(
        frame
            .rows
            .iter()
            .map(|cells| cells
                .iter()
                .map(|(text, _, _)| text.clone())
                .collect::<String>())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(frame.rows.iter().all(|cells| {
        cells
            .iter()
            .all(|(_, spot, ink)| spot.is_none() && matches!(ink, crate::swapped::Ink::Plain))
    }));
    let rows = screen(&app);
    assert!(rows.first().is_some_and(|row| row.starts_with(" Doctor")));
    assert!(rows.iter().any(|row| row.contains("second line")));

    let mut app = doctor_app(Some(fake()), report);
    app.attach(contract::SessionId(SESSION.to_owned()));
    slash_doctor(&mut app);
    assert!(app.config_view_open());
    let rows = screen(&app);
    assert!(rows.iter().any(|row| row.starts_with("  Doctor")));
    assert!(rows.iter().any(|row| row.contains("second line")));
}

#[test]
fn slash_doctor_keeps_long_lines_whole_and_clips_them_without_wrapping() {
    let report = format!("head:{}:tail", "x".repeat(120));
    let mut app = doctor_app(Some(fake()), &report);
    app.set_size(40, 24);
    slash_doctor(&mut app);

    let frame = app.config_view_screen().expect("the view is open");
    let row = frame.rows[0]
        .iter()
        .map(|(text, _, _)| text.as_str())
        .collect::<String>();
    assert_eq!(row, report, "the frame keeps the complete report line");

    let area = Rect::new(0, 0, 40, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let screen = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let report_row = screen
        .iter()
        .find(|line| line.contains("head:"))
        .expect("the report row is visible");
    assert_eq!(report_row.chars().count(), usize::from(area.width));
    assert_eq!(
        screen.iter().filter(|line| line.contains('x')).count(),
        1,
        "the line is clipped, not wrapped: {screen:?}"
    );
    assert!(!screen.iter().any(|line| line.contains(":tail")));
}

#[test]
fn slash_doctor_ignores_extra_args_and_clears_the_draft() {
    // Extra words change nothing: the view still opens, like
    // `/status` with rest, and the draft clears.
    let mut app = doctor_app(Some(fake()), "fiber 0.0.1");
    for ch in "/doctor extra".chars() {
        app.on_key(Key::Char(ch), now());
    }
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(app.config_view_open());
    assert!(app.input().is_empty());
}

#[test]
fn slash_doctor_esc_and_the_x_close_it_and_row_clicks_stay() {
    let mut app = doctor_app(Some(fake()), "fiber 0.0.1");
    slash_doctor(&mut app);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
    slash_doctor(&mut app);
    assert_eq!(app.on_click(TargetId::View(Spot::Close)), Effect::None);
    assert!(!app.config_view_open());
    // A report row takes no action: the view stays open.
    slash_doctor(&mut app);
    let selected = app
        .config_view_screen()
        .expect("the view is open")
        .list
        .selected();
    assert_eq!(app.on_click(TargetId::View(Spot::Row(0))), Effect::None);
    assert!(app.config_view_open());
    assert_eq!(
        app.config_view_screen()
            .expect("the view is open")
            .list
            .selected(),
        selected
    );
}

#[test]
fn slash_doctor_pages_a_report_taller_than_the_view() {
    let report = (0..60).map(|n| format!("line {n:02}")).collect::<Vec<_>>();
    let mut app = doctor_app(Some(fake()), &report.join("\n"));
    slash_doctor(&mut app);
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.list.selected(), 0);
    assert_eq!(frame.list.top(), 0);
    assert_eq!(frame.rows.len(), 60);
    // Up at the top stays: the selection clamps at both ends.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(
        app.config_view_screen()
            .expect("the view is open")
            .list
            .selected(),
        0
    );
    for _ in 0..3 {
        assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    }
    let selected = app
        .config_view_screen()
        .expect("the view is open")
        .list
        .selected();
    assert_eq!(selected, 3);
    // Paging down reaches the clamped bottom and scrolls the rows.
    for _ in 0..4 {
        assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    }
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.list.selected(), 59);
    assert!(frame.list.top() > 0);
    assert_eq!(app.on_key(Key::PageDown, now()), Effect::None);
    assert_eq!(
        app.config_view_screen()
            .expect("the view is open")
            .list
            .selected(),
        59
    );
    // Paging back up returns to the first row.
    for _ in 0..4 {
        assert_eq!(app.on_key(Key::PageUp, now()), Effect::None);
    }
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.list.selected(), 0);
    assert_eq!(frame.list.top(), 0);
    // The pasted edit changes nothing: the view has no field, and the
    // draft keeps nothing behind it.
    assert_eq!(app.on_edit(Edit::Paste("9".to_owned())), Effect::None);
    assert!(app.input().is_empty());
    assert!(app.config_view_open());
}

#[test]
fn slash_doctor_without_a_reader_says_not_available() {
    let mut app = home(Some(fake()));
    slash_doctor(&mut app);
    assert!(app.config_view_open());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Doctor");
    assert_eq!(frame.command, "/doctor");
    assert_eq!(
        frame.below,
        vec!["Not available in this terminal.".to_owned()]
    );
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.config_view_open());
}

#[test]
fn slash_doctor_capture_failure_keeps_a_notice_and_opens_nothing() {
    let mut app = home(Some(fake()));
    let failure = "could not capture the `fiber doctor` output: boom".to_owned();
    app.set_doctor(Some(Arc::new(move || Err(failure.clone()))));
    assert_eq!(slash_doctor(&mut app), Effect::None);
    assert!(!app.config_view_open());
    // The notice carries the capture text: the same sentence the
    // headless door prints.
    assert!(
        app.notices().iter().any(
            |notice| matches!(notice, crate::app::NoticeLine::Notice { code, text, .. } if code == "io_failed" && text == "could not capture the `fiber doctor` output: boom")
        ),
        "the failure notice names the capture text"
    );
}

/// An app on a listed hub with no local workspace: nothing is
/// attached and no directory is chosen.
fn remote_home(remote: Arc<crate::remote::Remote>, report: &str) -> App {
    let mut app = App::new(PathBuf::from("/client"));
    app.set_home(Launch {
        location: crate::LaunchLocation::Remote {
            hub: "box".to_owned(),
        },
        ..Default::default()
    });
    app.set_remote(remote);
    // A seam is present so the test would reach the hub-global read
    // without the early route; every Doctor input must skip it.
    app.set_configure(Some(fake() as Arc<dyn Configure>));
    app.set_doctor(Some(doctor_reader(report)));
    app.set_size(80, 24);
    app
}

#[test]
fn slash_doctor_on_a_remote_home_needs_no_workspace() {
    let hub = FailingHub::new();
    let report = (0..60).map(|n| format!("line {n:02}")).collect::<Vec<_>>();
    let mut app = remote_home(Arc::clone(&hub.remote), &report.join("\n"));
    assert_eq!(app.workspace(), None);
    // The view opens with no hub traffic: no seam read and no
    // workspace, local or hub-global.
    app.open_config_view(ConfigView::Doctor);
    assert!(app.config_view_open());
    assert!(hub.lines.try_recv().is_err());
    assert!(app.notices().is_empty());
    let frame = app.config_view_screen().expect("the view is open");
    assert_eq!(frame.title, "Doctor");
    assert_eq!(frame.rows.len(), 60);
    // Scrolling, edits and Esc work through the same route, still
    // with no hub traffic and no notice.
    assert_eq!(app.config_view_key(&Key::PageDown), Some(Effect::None));
    assert!(hub.lines.try_recv().is_err());
    let frame = app.config_view_screen().expect("the view is open");
    assert!(frame.list.selected() > 0);
    assert_eq!(
        app.config_view_edit(&Edit::Paste("9".to_owned())),
        Some(Effect::None)
    );
    assert!(hub.lines.try_recv().is_err());
    assert!(app.input().is_empty());
    assert!(app.config_view_open());
    // A report-row click stays inert through the same route: no hub
    // traffic, no notice, and the view still open, though the skipped
    // hub-global read would fail.
    assert_eq!(app.config_view_click(Spot::Row(0)), Effect::None);
    assert!(hub.lines.try_recv().is_err());
    assert!(app.notices().is_empty());
    assert!(app.config_view_open());
    assert_eq!(app.config_view_key(&Key::Esc), Some(Effect::None));
    assert!(!app.config_view_open());
    assert!(app.notices().is_empty());
    // And the hub read the view skipped would have failed.
    let error = hub.reject_global_file();
    assert_eq!(error.code, contract::ErrorCode::StaleRequest);
}

/// The wheel shifts the visible rows three a step, keeping a visible
/// selection where it was, with no seam or workspace involved
/// (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_shifts_three_rows_and_keeps_a_visible_selection() {
    use super::doctor_view::Doctor;
    use crate::settings_view::Act;
    let report = (0..60)
        .map(|n| format!("line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut view = Doctor::open(&report);
    for _ in 0..10 {
        assert!(matches!(view.key(&Key::Down, 24), Act::Stay));
    }
    assert_eq!(
        (view.frame().list.top(), view.frame().list.selected()),
        (0, 10)
    );
    assert!(matches!(view.wheel(false, 24), Act::Stay));
    assert_eq!(view.frame().list.top(), 3);
    assert_eq!(
        view.frame().list.selected(),
        10,
        "a visible selection stays"
    );
}
