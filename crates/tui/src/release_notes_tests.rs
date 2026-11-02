//! Tests for `/release-notes`: the exact running-version section
//! selection, the notes and fallback frames, and the app's local
//! open/scroll/close path (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;

use super::{frame, section};
use crate::app::{App, Effect, SessionView};
use crate::keys::{Edit, Key};
use crate::mouse::TargetId;
use crate::swapped::{Frame, Ink, List, Spot as ViewSpot};
use crate::theme::Role;
use contract::SessionId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const VERSION: &str = "0.0.1";
const OTHER: &str = "0.0.2";
const NIGHTLY: &str = "0.0.1-nightly.1";
const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// Two adjacent sections around one long entry, with a longer version
/// after it: the bullet, paragraph and URL each end in a unique tail
/// sentinel the tests page to.
static SOURCE: &str = "# Changelog\n\n## 0.0.2\n\n- Second version first bullet.\n\n## 0.0.1\n\n### Breaking changes\n\n- The old flag is gone; use the new flag instead.\n\n### New features\n\n- A long bullet that keeps going past the available width so wrapping must carry its tail onto hanging rows below the first, ending with the sentinel alpha-bravo-charlie-delta.\n- Café 日本語 🎉 tail-unicode-sierra.\n\nPlain paragraph text that also runs long enough to wrap across several rows at narrow widths, ending with the sentinel echo-foxtrot-golf-hotel.\n\nhttps://example.com/a/very/long/path/that/never/breaks/at/a/space/so/the/wrapper/must/split/it/ending-with-india-juliet-kilo-lima\n\n## 0.0.10\n\n- A longer version's bullet that must never leak into 0.0.1.\n";

/// The title-only stub the binary embeds until the first section lands.
static STUB: &str = "# Changelog\n";

fn sel(source: &str, version: &str) -> Option<String> {
    section(source, version).map(str::to_owned)
}

#[test]
fn exact_heading_selects_only_its_section() {
    let body = sel(SOURCE, VERSION).expect("the exact section");
    assert!(body.contains("alpha-bravo-charlie-delta"));
    assert!(!body.contains("Second version first bullet"));
    assert!(!body.contains("longer version's bullet"));
    assert!(body.contains("### Breaking changes"));
    assert!(body.contains("### New features"));
}

#[test]
fn trailing_space_does_not_match() {
    let source = "# Changelog\n\n## 0.0.1 \n\n- Padded.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn trailing_tab_does_not_match() {
    let source = "# Changelog\n\n## 0.0.1\t\n\n- Padded.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn leading_space_does_not_match() {
    let source = "# Changelog\n\n ## 0.0.1\n\n- Padded.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn brackets_do_not_match() {
    let source = "# Changelog\n\n## [0.0.1]\n\n- Bracketed.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn v_prefix_does_not_match() {
    let source = "# Changelog\n\n## v0.0.1\n\n- Prefixed.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn date_suffix_does_not_match() {
    let source = "# Changelog\n\n## 0.0.1 - 2026-10-26\n\n- Dated.\n";
    assert_eq!(sel(source, VERSION), None);
}

#[test]
fn longer_version_does_not_match() {
    assert!(
        !sel(SOURCE, VERSION)
            .expect("section")
            .contains("longer version")
    );
    let only_longer = "# Changelog\n\n## 0.0.10\n\n- Longer.\n";
    assert_eq!(sel(only_longer, VERSION), None);
}

#[test]
fn nightly_without_an_exact_heading_selects_nothing() {
    assert_eq!(sel(SOURCE, NIGHTLY), None);
}

#[test]
fn title_stub_selects_nothing() {
    assert_eq!(sel(STUB, VERSION), None);
    assert_eq!(sel(STUB, OTHER), None);
}

#[test]
fn section_runs_to_eof_without_a_final_newline() {
    let source = "# Changelog\n\n## 0.0.2\n\n- Newer.\n\n## 0.0.1\n\n- Tail without newline.";
    assert_eq!(
        sel(source, VERSION),
        Some("\n- Tail without newline.".to_owned())
    );
}

#[test]
fn first_exact_heading_wins() {
    let source = "# Changelog\n\n## 0.0.1\n\n- First.\n\n## 0.0.1\n\n- Second.\n";
    let body = sel(source, VERSION).expect("the first section");
    assert!(body.contains("First"));
    assert!(!body.contains("Second"));
}

#[test]
fn level_two_prefix_without_space_does_not_end_the_section() {
    let source =
        "# Changelog\n\n## 0.0.1\n\nKeep ##x here.\n\n##\n\nStill here.\n\n## 0.0.2\n\n- Newer.\n";
    let body = sel(source, VERSION).expect("the section");
    assert!(body.contains("Keep ##x here."));
    assert!(body.contains("Still here."));
    assert!(!body.contains("Newer"));
}

/// The frame's text rows, cells joined: comparing these against the
/// section proves wrapping kept every word.
fn text_rows(shown: &Frame) -> Vec<String> {
    shown
        .rows
        .iter()
        .map(|cells| cells.iter().map(|(text, _, _)| text.as_str()).collect())
        .collect()
}

/// The rows with whitespace and the bullet marker folded away: equal
/// exactly when no text was lost or added by wrapping.
fn squashed(shown: &Frame) -> String {
    text_rows(shown)
        .join("")
        .replace('•', "-")
        .chars()
        .filter(|cell| !cell.is_whitespace())
        .collect()
}

/// The section body folded the same way: the assertion target.
fn squashed_body(body: &str) -> String {
    body.chars().filter(|cell| !cell.is_whitespace()).collect()
}

#[test]
fn frame_carries_the_view_title_and_command() {
    let shown = frame(SOURCE, VERSION, List::default(), 80, 24);
    assert_eq!(shown.title, "Release notes");
    assert_eq!(shown.command, "/release-notes");
    assert!(shown.field.is_none());
}

#[test]
fn wrapping_loses_no_text() {
    for width in [80, 48, 30, 20, 12] {
        let shown = frame(SOURCE, VERSION, List::default(), width, 100);
        assert_eq!(
            squashed(&shown),
            squashed_body(section(SOURCE, VERSION).expect("section")),
            "every word survives at width {width}"
        );
    }
}

#[test]
fn long_bullet_has_the_accent_prefix_and_hanging_continuations() {
    let shown = frame(SOURCE, VERSION, List::default(), 48, 100);
    let rows = text_rows(&shown);
    let at = rows
        .iter()
        .position(|row| row.starts_with("• A long bullet"))
        .expect("the long bullet's first row");
    assert_eq!(
        shown.rows[at][0],
        ("• ".to_owned(), None, Ink::Role(Role::Accent))
    );
    assert_eq!(shown.rows[at][1].2, Ink::Plain);
    assert!(rows.len() > at + 2, "the bullet wraps: {rows:?}");
    let mut wrapped = 0;
    for cells in shown.rows.iter().skip(at + 1) {
        if cells.len() == 2 && cells[0].0 == "  " {
            assert_eq!(cells[0].2, Ink::Plain);
            wrapped += 1;
        } else {
            break;
        }
    }
    assert!(wrapped >= 2, "the bullet hangs its rows: {rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.contains("alpha-bravo-charlie-delta")),
        "the bullet's tail survives: {rows:?}"
    );
}

#[test]
fn bullet_body_wraps_in_the_width_left_after_its_prefix() {
    // The body fits exactly in 48 - 2: one row, then the blank line.
    let line = format!("- {}", "b".repeat(46));
    let source = format!("# Changelog\n\n## {VERSION}\n{line}\n");
    let one = frame(&source, VERSION, List::default(), 48, 100);
    assert_eq!(
        text_rows(&one),
        vec![format!("• {}", "b".repeat(46)), String::new()]
    );
    // One cell more wraps: the two-column prefix is counted.
    let line = format!("- {}", "b".repeat(47));
    let source = format!("# Changelog\n\n## {VERSION}\n{line}\n");
    let two = frame(&source, VERSION, List::default(), 48, 100);
    assert_eq!(
        text_rows(&two),
        vec![
            format!("• {}", "b".repeat(46)),
            format!("  {}", "b"),
            String::new()
        ]
    );
}

#[test]
fn ordinary_line_wraps_at_the_full_width() {
    let line = "c".repeat(48);
    let source = format!("# Changelog\n\n## {VERSION}\n{line}\n");
    let one = frame(&source, VERSION, List::default(), 48, 100);
    assert_eq!(text_rows(&one), vec![line, String::new()]);
    let line = "d".repeat(49);
    let source = format!("# Changelog\n\n## {VERSION}\n{line}\n");
    let two = frame(&source, VERSION, List::default(), 48, 100);
    assert_eq!(
        text_rows(&two),
        vec!["d".repeat(48), "d".to_owned(), String::new()]
    );
}

#[test]
fn dash_without_space_is_an_ordinary_line() {
    let source = format!("# Changelog\n\n## {VERSION}\n\n-x marks nothing.\n-\n");
    let shown = frame(&source, VERSION, List::default(), 80, 100);
    let rows = text_rows(&shown);
    assert!(rows.iter().any(|row| row == "-x marks nothing."));
    assert!(!rows.iter().any(|row| row.starts_with('•')));
}

#[test]
fn long_url_splits_and_keeps_its_tail() {
    let shown = frame(SOURCE, VERSION, List::default(), 48, 100);
    let rows = text_rows(&shown);
    assert!(
        rows.iter()
            .any(|row| row.contains("india-juliet-kilo-lima")),
        "the url tail survives: {rows:?}"
    );
    let last = rows
        .iter()
        .rfind(|row| !row.is_empty())
        .expect("a last row");
    assert!(
        last.contains("india-juliet-kilo-lima"),
        "the url's tail is the section's last text: {last}"
    );
}

#[test]
fn blank_lines_and_unicode_survive() {
    let shown = frame(SOURCE, VERSION, List::default(), 48, 100);
    let rows = text_rows(&shown);
    assert!(rows.iter().any(|row| row.is_empty()), "blank lines stay");
    assert!(
        rows.iter().any(|row| row.contains("tail-unicode-sierra")),
        "unicode survives: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Café") && row.contains("日本語"))
    );
}

#[test]
fn fallback_names_the_releases_page() {
    let shown = frame(STUB, VERSION, List::default(), 80, 24);
    let rows = text_rows(&shown);
    assert!(
        rows.iter()
            .any(|row| row.contains("No notes for this version"))
    );
    assert!(
        rows.iter()
            .any(|row| row.contains(contract::releases::RELEASES))
    );
    let nightly = frame(SOURCE, NIGHTLY, List::default(), 80, 24);
    let rows = text_rows(&nightly);
    assert!(
        rows.iter()
            .any(|row| row.contains("No notes for this version"))
    );
    assert!(
        rows.iter()
            .any(|row| row.contains(contract::releases::RELEASES))
    );
}

#[test]
fn frame_clamps_a_list_stranded_past_its_last_row() {
    let mut list = List::default();
    list.select(999, usize::MAX, usize::MAX);
    let shown = frame(SOURCE, VERSION, list, 48, 24);
    let last = shown.rows.len().saturating_sub(1);
    assert_eq!(shown.list.selected(), last);
    let visible = crate::swapped::rows_height(&shown, 24);
    assert_eq!(shown.list.top() + visible, shown.rows.len());
}

#[test]
fn empty_section_draws_one_blank_row() {
    let source = format!("# Changelog\n\n## {VERSION}\n## {OTHER}\n\n- Newer.\n");
    let shown = frame(&source, VERSION, List::default(), 80, 24);
    assert_eq!(text_rows(&shown), vec![String::new()]);
}

fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

fn launch(version: &str, notes: &'static str) -> crate::home::Launch {
    crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        version: version.to_owned(),
        release_notes: notes,
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    }
}

fn home_app(width: u16, height: u16, version: &str, notes: &'static str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch(version, notes));
    app.set_size(width, height);
    app
}

fn attached_app(width: u16, height: u16, version: &str, notes: &'static str) -> App {
    let mut app = home_app(width, height, version, notes);
    app.attach(SessionId(SESSION.to_owned()));
    app
}

fn screen(app: &App, width: u16, height: u16) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buffer, None);
    (crate::view::text(&buffer), targets)
}

/// Types `/release-notes` and presses Enter: opening sends nothing.
fn slash_notes(app: &mut App) -> Effect {
    app.on_edit(Edit::Paste("/release-notes".to_owned()));
    app.on_key(Key::Enter, now())
}

fn open_notes(app: &mut App) {
    assert_eq!(
        app.open_session_view(SessionView::ReleaseNotes),
        Effect::None
    );
}

#[test]
fn slash_notes_opens_on_home_clears_the_draft_and_sends_nothing() {
    let mut app = home_app(80, 24, VERSION, SOURCE);
    assert_eq!(slash_notes(&mut app), Effect::None);
    assert!(app.session_view_open());
    assert!(app.input().is_empty());
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Release notes"));
    assert!(shown.contains("The old flag is gone"));
    assert!(!shown.contains("Second version first bullet"));
}

#[test]
fn slash_notes_opens_attached_and_reads_the_client_version() {
    let mut app = attached_app(80, 24, VERSION, SOURCE);
    assert_eq!(slash_notes(&mut app), Effect::None);
    assert!(app.session_view_open());
    assert!(app.input().is_empty());
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("Release notes"));
    assert!(shown.contains("The old flag is gone"));
}

#[test]
fn remote_terminal_still_reads_the_client_launch() {
    let mut app = App::new(PathBuf::from("/w"));
    let mut started = launch(VERSION, SOURCE);
    started.location = crate::LaunchLocation::Remote {
        hub: "box".to_owned(),
    };
    app.set_home(started);
    app.set_size(80, 24);
    app.attach(SessionId(SESSION.to_owned()));
    assert_eq!(slash_notes(&mut app), Effect::None);
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("The old flag is gone"));
    assert!(!shown.contains("Second version first bullet"));
}

#[test]
fn stub_and_nightly_show_the_fallback_with_the_url() {
    let mut app = home_app(80, 24, VERSION, STUB);
    assert_eq!(slash_notes(&mut app), Effect::None);
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("No notes for this version"));
    assert!(shown.contains(contract::releases::RELEASES));
    let mut app = home_app(80, 24, NIGHTLY, SOURCE);
    assert_eq!(slash_notes(&mut app), Effect::None);
    let shown = screen(&app, 80, 24).0;
    assert!(shown.contains("No notes for this version"));
    assert!(shown.contains(contract::releases::RELEASES));
}

#[test]
fn notes_open_without_a_session_while_status_still_rejects() {
    let mut app = home_app(80, 24, VERSION, SOURCE);
    app.on_edit(Edit::Paste("/status".to_owned()));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(!app.session_view_open());
    assert!(screen(&app, 80, 24).0.contains("No session on screen."));
    assert_eq!(slash_notes(&mut app), Effect::None);
    assert!(app.session_view_open());
}

#[test]
fn escape_closes_and_reopening_starts_at_the_top() {
    let mut app = attached_app(80, 24, VERSION, SOURCE);
    open_notes(&mut app);
    app.on_key(Key::Down, now());
    app.on_key(Key::Down, now());
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 2);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.session_view_open());
    open_notes(&mut app);
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 0);
}

#[test]
fn header_close_returns() {
    let mut app = attached_app(80, 24, VERSION, SOURCE);
    open_notes(&mut app);
    let (_, targets) = screen(&app, 80, 24);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(ViewSpot::Close))
    );
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Close)), Effect::None);
    assert!(!app.session_view_open());
}

#[test]
fn row_click_selects_and_pages_reach_the_wrapped_tail() {
    let mut app = attached_app(80, 24, VERSION, SOURCE);
    open_notes(&mut app);
    let (_, targets) = screen(&app, 80, 24);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(ViewSpot::Row(2)))
    );
    assert_eq!(app.on_click(TargetId::View(ViewSpot::Row(2))), Effect::None);
    assert_eq!(app.session_view_screen(0).unwrap().list.selected(), 2);
    let rows = app.session_view_screen(0).unwrap().rows.len();
    for _ in 0..rows + 2 {
        app.on_key(Key::PageDown, now());
    }
    let shown = app.session_view_screen(0).unwrap();
    assert_eq!(shown.list.selected(), rows.saturating_sub(1));
    let text = screen(&app, 80, 24).0;
    assert!(text.contains("india-juliet-kilo-lima"));
}

#[test]
fn resize_keeps_the_tail_and_clamps_a_stale_selection() {
    let mut app = attached_app(80, 24, VERSION, SOURCE);
    open_notes(&mut app);
    app.set_size(50, 14);
    let narrow_rows = app.session_view_screen(0).unwrap().rows.len();
    for _ in 0..narrow_rows + 2 {
        app.on_key(Key::PageDown, now());
    }
    let tailed = app.session_view_screen(0).unwrap();
    assert_eq!(tailed.list.selected(), narrow_rows.saturating_sub(1));
    assert!(screen(&app, 50, 14).0.contains("india-juliet-kilo-lima"));
    // Widening shortens the wrapped list under the stored selection:
    // the redrawn frame clamps it, so the tail stays on screen with
    // no further key.
    app.set_size(80, 26);
    let widened = app.session_view_screen(0).unwrap();
    assert_eq!(
        widened.list.selected(),
        widened.rows.len().saturating_sub(1)
    );
    assert!(screen(&app, 80, 26).0.contains("india-juliet-kilo-lima"));
    // Narrowing again leaves a valid selection and a nonblank view.
    app.set_size(50, 14);
    let narrowed = app.session_view_screen(0).unwrap();
    assert!(narrowed.list.selected() < narrowed.rows.len());
    assert!(screen(&app, 50, 14).0.contains("Release notes"));
}
