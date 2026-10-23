//! Tests for the not-installed rows' drawing: the four states of the
//! prototype with real data, the colours, the window and the targets
//! (`docs/tui.md`, "Swapped views", "Overlays", "Choices").

use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::super::{body, draw, model_fit, page_step};
use crate::configure::{Configure, ConfigureError, LoginNeed, ProviderInstall};
use crate::configure_fake::Fake;
use crate::model_picker::fixtures::catalogue;
use crate::model_picker::{InstallStart, Mode, ModelPicker, PickerCtx, PickerView};
use crate::mouse::{Target, TargetId};
use crate::swapped::Spot;
use crate::theme::Role;

/// An open choosing picker over `models` with `installable` not installed.
fn picker(models: &[&str], installable: &[&str]) -> ModelPicker {
    let mut picker = ModelPicker::default();
    picker.store(Ok(catalogue(models, installable)));
    picker.open(Mode::Choose, None);
    picker
}

fn start(provider: &str) -> InstallStart {
    InstallStart {
        provider: provider.to_owned(),
        workspace: "/w".into(),
        seam: Arc::new(Fake::new(Vec::new())) as Arc<dyn Configure>,
    }
}

/// Lands `provider`'s install on the open picker with `summary` and `login`.
fn install(picker: &mut ModelPicker, provider: &str, summary: &[&str], login: Vec<LoginNeed>) {
    assert!(picker.installs.start(start(provider)));
    let done = ProviderInstall {
        installed: Vec::new(),
        summary: summary.iter().map(|line| (*line).to_owned()).collect(),
        login,
    };
    let _ = picker.land_install(provider.to_owned(), Ok(done));
}

fn view_of(picker: &ModelPicker) -> PickerView {
    picker
        .view(&PickerCtx {
            usage: None,
            wall_ms: 0,
            spinner: "●",
            current: None,
            home_only: None,
        })
        .expect("the picker is open")
}

fn render(picker: &ModelPicker, width: u16, height: u16) -> (Buffer, Vec<Target>, Rect) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let slab = draw(&view_of(picker), area, &mut buf, &mut targets);
    (buf, targets, slab)
}

fn screen(picker: &ModelPicker, width: u16, height: u16) -> String {
    crate::view::text(&render(picker, width, height).0)
}

/// The screen row holding `needle`, and its text.
fn find_row(text: &str, needle: &str) -> (u16, String) {
    let (at, line) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("no row holds {needle:?}: {text}"));
    (u16::try_from(at).unwrap_or(u16::MAX), (*line).to_owned())
}

/// The cell column of `needle` in `line`.
fn cell_x(line: &str, needle: &str) -> u16 {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle:?} in {line:?}"));
    u16::try_from(crate::format::width(&line[..byte])).unwrap_or(u16::MAX)
}

fn cell_at<'a>(buf: &'a Buffer, text: &str, needle: &str) -> &'a ratatui::buffer::Cell {
    let (y, line) = find_row(text, needle);
    &buf[(cell_x(&line, needle), y)]
}

fn barred(buf: &Buffer) -> Vec<u16> {
    let area = buf.area;
    (area.top()..area.bottom())
        .filter(|y| (area.left()..area.right()).any(|x| buf[(x, *y)].bg == Role::Accent.color()))
        .collect()
}

fn row_text(row: &super::super::Row) -> String {
    row.spans.iter().map(|span| span.content.as_ref()).collect()
}

fn fail_with(picker: &mut ModelPicker, provider: &str, message: &str) {
    assert!(picker.installs.start(start(provider)));
    let error = ConfigureError {
        code: contract::ErrorCode::IoFailed,
        message: message.to_owned(),
    };
    let _ = picker.land_install(provider.to_owned(), Err(error));
}

fn missing(provider: &str) -> LoginNeed {
    LoginNeed {
        provider: provider.to_owned(),
        error: None,
    }
}

#[test]
fn snapshot_missing() {
    let picker = picker(&["acme/m1", "zeta/z1"], &["databricks"]);
    insta::assert_snapshot!("model_picker_install_missing", screen(&picker, 100, 30));
}

#[test]
fn snapshot_installing() {
    let mut picker = picker(&["acme/m1", "zeta/z1"], &["databricks"]);
    assert!(picker.installs.start(start("databricks")));
    insta::assert_snapshot!("model_picker_install_installing", screen(&picker, 100, 30));
}

#[test]
fn snapshot_failed() {
    let mut picker = picker(&["acme/m1", "zeta/z1"], &["databricks"]);
    fail_with(
        &mut picker,
        "databricks",
        "Could not fetch https://example.invalid: timed out",
    );
    insta::assert_snapshot!("model_picker_install_failed", screen(&picker, 100, 30));
}

#[test]
fn snapshot_done() {
    let mut picker = picker(&["acme/m1", "zeta/z1"], &["databricks"]);
    let summary = [
        "Install databricks from https://example.invalid/databricks",
        "Version v1.4.0",
        "Provider databricks: https://api.databricks.invalid",
    ];
    install(
        &mut picker,
        "databricks",
        &summary,
        vec![missing("databricks")],
    );
    picker.store(Ok(catalogue(
        &["acme/m1", "databricks/dbrx", "zeta/z1"],
        &[],
    )));
    insta::assert_snapshot!("model_picker_install_done", screen(&picker, 100, 30));
}

#[test]
fn snapshot_done_unreadable() {
    let mut picker = picker(&["acme/m1", "zeta/z1"], &["databricks"]);
    let login = vec![LoginNeed {
        provider: "databricks".to_owned(),
        error: Some("the keyring is locked".to_owned()),
    }];
    install(&mut picker, "databricks", &["Version v1.4.0"], login);
    insta::assert_snapshot!(
        "model_picker_install_done_unreadable",
        screen(&picker, 100, 30)
    );
}

#[test]
fn the_row_reads_in_the_prototypes_colours() {
    let picker = picker(&["acme/m1"], &["databricks"]);
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let name = cell_at(&buf, &text, "databricks");
    assert_eq!(name.fg, Role::Accent.color());
    let mark = cell_at(&buf, &text, "not installed");
    assert!(mark.modifier.contains(Modifier::DIM));
    assert_ne!(mark.fg, Role::Accent.color());
}

#[test]
fn an_installing_row_reads_installing_with_the_spinner_in_attention() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    assert!(picker.installs.start(start("databricks")));
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let (_, line) = find_row(&text, "databricks");
    assert!(line.contains("databricks ⟳ installing ●"), "{line}");
    assert!(!line.contains("not installed"), "{line}");
    assert_eq!(
        cell_at(&buf, &text, "⟳ installing").fg,
        Role::Attention.color()
    );
    assert_eq!(cell_at(&buf, &text, "●").fg, Role::Attention.color());
}

#[test]
fn a_failed_row_keeps_not_installed_and_adds_the_error_in_error() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    fail_with(&mut picker, "databricks", "no network");
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let (row, line) = find_row(&text, "databricks");
    assert!(line.contains("not installed"), "{line}");
    let (error_at, error) = find_row(&text, "✗ no network");
    assert_eq!(error_at, row + 1);
    let indent = error
        .find('✗')
        .map(|byte| crate::format::width(&error[..byte]));
    let name = cell_x(&line, "databricks");
    assert_eq!(
        indent,
        Some(usize::from(name) + 2),
        "four columns in from the gutter: {error}"
    );
    assert_eq!(cell_at(&buf, &text, "✗ no network").fg, Role::Error.color());
    assert_eq!(cell_at(&buf, &text, "no network").fg, Role::Error.color());
}

#[test]
fn a_done_install_draws_its_outcome_lines() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    install(
        &mut picker,
        "databricks",
        &["Install databricks", "Version v1.4.0"],
        vec![missing("databricks")],
    );
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    assert!(!text.contains("not installed"), "{text}");
    assert_eq!(
        cell_at(&buf, &text, "✓ installed databricks").fg,
        Role::Success.color()
    );
    let installed = cell_at(&buf, &text, "installed databricks");
    assert_ne!(installed.fg, Role::Success.color());
    assert!(!installed.modifier.contains(Modifier::DIM));
    let summary = cell_at(&buf, &text, "Version v1.4.0");
    assert!(summary.modifier.contains(Modifier::DIM));
    let need = cell_at(
        &buf,
        &text,
        "databricks needs a credential: run `fiber login databricks`",
    );
    assert!(need.modifier.contains(Modifier::DIM));
    let (_, ok) = find_row(&text, "✓ installed databricks");
    let (_, line) = find_row(&text, "Version v1.4.0");
    assert_eq!(
        cell_x(&ok, "✓"),
        cell_x(&line, "Version v1.4.0"),
        "one indent: {ok:?} {line:?}"
    );
}

#[test]
fn an_unreadable_key_draws_in_error_and_never_as_missing() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    let login = vec![LoginNeed {
        provider: "databricks".to_owned(),
        error: Some("the keyring is locked".to_owned()),
    }];
    install(&mut picker, "databricks", &[], login);
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    assert!(!text.contains("needs a credential"), "{text}");
    let line = "✗ databricks's credential could not be read: the keyring is locked";
    assert_eq!(cell_at(&buf, &text, line).fg, Role::Error.color());
    assert_eq!(
        cell_at(&buf, &text, "keyring is locked").fg,
        Role::Error.color()
    );
}

#[test]
fn exactly_one_row_is_barred_with_a_model_focused_and_with_a_row_focused() {
    let mut picker = picker(&["acme/m1"], &["databricks", "zeta"]);
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let bars = barred(&buf);
    assert_eq!(bars.len(), 1, "{bars:?}");
    let (model, _) = find_row(&text, "m1");
    assert_eq!(bars, [model]);
    let (_, databricks) = find_row(&text, "databricks");
    assert!(databricks.contains("  databricks"), "{databricks}");
    assert!(!databricks.contains("› databricks"), "{databricks}");

    picker.move_row(1);
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let (at, focused) = find_row(&text, "databricks");
    assert_eq!(barred(&buf), [at]);
    assert!(focused.contains("› databricks"), "{focused}");
    let (_, other) = find_row(&text, "zeta");
    assert!(other.contains("  zeta"), "{other}");
    assert!(!other.contains("› zeta"), "{other}");
    let (_, model) = find_row(&text, "m1");
    assert!(!model.contains('›'), "{model}");
}

#[test]
fn a_blank_row_sets_the_first_row_off_from_the_sections_and_none_from_the_second() {
    let picker = picker(&["acme/m1"], &["databricks", "zeta"]);
    let rows = body(&view_of(&picker), 100, 30);
    let at = rows
        .iter()
        .position(|row| row_text(row).contains("databricks"))
        .unwrap();
    assert!(rows[at - 1].spans.is_empty(), "a blank row above the first");
    assert!(
        row_text(&rows[at + 1]).contains("zeta"),
        "none above the second"
    );
    assert!(!rows[at + 1 - 1].spans.is_empty());

    let none = self::picker(&[], &["databricks", "zeta"]);
    let rows = body(&view_of(&none), 100, 30);
    let at = rows
        .iter()
        .position(|row| row_text(row).contains("databricks"))
        .unwrap();
    assert!(
        !rows[at - 1].spans.is_empty(),
        "no blank row with no models"
    );
    assert_eq!(row_text(&rows[at - 1]), "0 models", "the buttons row");
}

#[test]
fn each_provider_gets_one_row_below_the_models() {
    let picker = picker(&["acme/m1"], &["anthropic", "openai", "zeta"]);
    let text = screen(&picker, 100, 30);
    for name in ["anthropic", "openai", "zeta"] {
        let rows = text
            .lines()
            .filter(|line| line.contains(&format!("{name} · not installed")))
            .count();
        assert_eq!(rows, 1, "{name}: {text}");
        assert!(find_row(&text, "m1").0 < find_row(&text, name).0, "{text}");
    }
}

#[test]
fn the_focused_row_stays_on_screen_at_every_position_on_a_short_terminal() {
    let names: Vec<String> = (1..=8).map(|n| format!("prov{n}")).collect();
    let installable: Vec<&str> = names.iter().map(String::as_str).collect();
    for with_models in [false, true] {
        let models: &[&str] = if with_models { &["acme/m1"] } else { &[] };
        let mut picker = picker(models, &installable);
        let steps = names.len() + usize::from(with_models);
        for step in 0..steps {
            let text = screen(&picker, 80, 12);
            let focus = picker.install_target();
            if let Some(name) = focus {
                assert!(
                    text.contains(&format!("› {name} ")),
                    "step {step}, models {with_models}: {text}"
                );
            } else if with_models {
                assert!(text.contains("› m1"), "step {step}: {text}");
            }
            picker.move_row(1);
        }
        assert_eq!(picker.install_target().as_deref(), Some("prov8"));
    }
}

#[test]
fn the_page_step_counts_rows_and_an_error_takes_two() {
    let names: Vec<String> = (1..=8).map(|n| format!("prov{n}")).collect();
    let installable: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut picker = picker(&[], &installable);
    // 12 rows less the chrome, the two rows above the list and the
    // "No models" status line.
    assert_eq!(page_step(&view_of(&picker), 12), 3);
    fail_with(&mut picker, "prov1", "boom");
    // The failed row and its error take two of the three.
    assert_eq!(page_step(&view_of(&picker), 12), 2);
}

#[test]
fn outcome_lines_reserve_their_rows_and_never_hide_the_focus() {
    let names: Vec<String> = (1..=8).map(|n| format!("prov{n}")).collect();
    let installable: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut picker = picker(&[], &installable);
    let bare = model_fit(&view_of(&picker), 30);
    install(
        &mut picker,
        "prov1",
        &["one", "two", "three"],
        vec![missing("prov1")],
    );
    // The outcome line, its three summary lines and the credential line.
    assert_eq!(model_fit(&view_of(&picker), 30), bare - 5);
    for _ in 0..7 {
        picker.move_row(1);
    }
    assert_eq!(picker.install_target().as_deref(), Some("prov8"));
    let text = screen(&picker, 80, 14);
    assert!(text.contains("› prov8 "), "{text}");
    assert!(text.contains("✓ installed prov1"), "{text}");
}

#[test]
fn a_row_takes_a_row_target_and_a_name_target_and_its_error_only_a_row_target() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    fail_with(&mut picker, "databricks", "boom");
    let view = view_of(&picker);
    let at = view.installs[0].at;
    let (buf, targets, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    let (row, line) = find_row(&text, "databricks");
    let on = |y: u16| -> Vec<&Target> { targets.iter().filter(|t| t.rect.y == y).collect() };
    let row_targets = on(row);
    let whole = row_targets
        .iter()
        .find(|t| t.id == TargetId::View(Spot::Row(at)))
        .expect("a row target");
    assert!(whole.rect.width > 40, "{whole:?}");
    let name = row_targets
        .iter()
        .find(|t| t.id == TargetId::View(Spot::Cell(at, 0)))
        .expect("a name target");
    assert_eq!(name.rect.x, cell_x(&line, "databricks"));
    assert_eq!(name.rect.width, 10);
    let error = on(row + 1);
    assert!(
        error.iter().all(|t| t.id == TargetId::View(Spot::Row(at))),
        "{error:?}"
    );
    assert!(!error.is_empty());
}

#[test]
fn an_error_draws_its_first_line_with_no_escape() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    fail_with(&mut picker, "databricks", "bad\u{1b}[31m\nsecond line");
    let (buf, _, _) = render(&picker, 100, 30);
    let text = crate::view::text(&buf);
    assert!(text.contains("✗ bad [31m"), "{text}");
    assert!(!text.contains("second line"), "{text}");
    assert!(!text.contains('\u{1b}'));
    for y in buf.area.top()..buf.area.bottom() {
        for x in buf.area.left()..buf.area.right() {
            assert!(!buf[(x, y)].symbol().contains('\u{1b}'));
        }
    }
}

#[test]
fn an_error_is_cut_at_eighty_columns_and_the_picker_does_not_widen() {
    let width_of = |message: &str| {
        let mut picker = picker(&["acme/m1"], &["databricks"]);
        fail_with(&mut picker, "databricks", message);
        let (buf, _, slab) = render(&picker, 100, 30);
        let text = crate::view::text(&buf);
        let (_, line) = find_row(&text, "✗ ");
        (line, slab.width)
    };
    let (exact, _) = width_of(&"x".repeat(80));
    assert!(exact.contains(&format!("✗ {}", "x".repeat(80))), "{exact}");
    assert!(!exact.contains('…'), "{exact}");
    let (over, over_width) = width_of(&"x".repeat(81));
    assert!(over.contains(&format!("✗ {}…", "x".repeat(79))), "{over}");
    assert!(!over.contains(&"x".repeat(80)), "{over}");
    let (_, long_width) = width_of(&"y".repeat(200));
    assert_eq!(long_width, over_width);
    assert!(long_width <= 96 + 2, "{long_width}");
}

#[test]
fn a_long_outcome_line_is_cut_the_same_way() {
    let mut picker = picker(&["acme/m1"], &["databricks"]);
    let long = "s".repeat(120);
    install(&mut picker, "databricks", &[&long], Vec::new());
    let text = screen(&picker, 100, 30);
    assert!(text.contains(&format!("{}…", "s".repeat(79))), "{text}");
    assert!(!text.contains(&"s".repeat(80)), "{text}");
}
