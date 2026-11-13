//! Tests for `/settings`: the tabs, the control each key's type gives it,
//! where a change saves, the list sub-page, the cross-tab search, and
//! when a write applies
//! (`docs/tui.md`, "Swapped views", `/settings`; `docs/configuration.md`,
//! "When Fiber reads configuration"). Screens are drawn through the real
//! frame renderer and compared whole.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{
    Act, Applies, Ctx, Request, SCREEN_ROWS, Screen, ScreenRow, Settings, applies, applies_text,
    as_typed, layers, next_number_value, next_text_value, number_list, number_slider,
    refresh_millis, shown_value, slider_bar, text_list,
};
use crate::ThemeSetting;
use crate::configure::{Layer, SettingControl, SettingRow, Shown, WriteScope};
use crate::configure_fake::{Fake, controlled, file, placed, row};
use crate::keys::{Edit, Key};
use crate::swapped::{Spot, render};
use contract::settings::SettingsTab;

/// A value shown as compact JSON.
fn value(json: &str) -> Shown {
    Shown::Value(json.to_owned())
}

const ANY: WriteScope = WriteScope::Any { repo: true };
const PERSON: WriteScope = WriteScope::PersonFiles;

/// A row on `tab`, labelled `label`, edited by `control`.
fn on(
    tab: SettingsTab,
    label: &str,
    key: &str,
    shown: Shown,
    layer: &str,
    scope: WriteScope,
    control: SettingControl,
) -> SettingRow {
    placed(controlled(key, shown, layer, scope, control), tab, label)
}

fn fixed(names: &[&str]) -> SettingControl {
    SettingControl::Fixed(names.iter().map(|name| (*name).to_owned()).collect())
}

/// The rows the tests read, one or more per tab, by control type and by
/// write scope, sorted by key as `Config::settings` lists them. The
/// controls are stated here: nothing derives one from a key.
fn rows() -> Vec<SettingRow> {
    let mut rows = unsorted();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

fn unsorted() -> Vec<SettingRow> {
    use SettingControl::{Boolean, Model, Number, RepositoryExtensions, StringList, Text, Theme};
    use SettingsTab::{Advanced, Appearance, Models, Notifications, Permissions, Sessions};
    vec![
        on(
            Advanced,
            "Diagnostics level",
            "diagnostics.level",
            value("\"info\""),
            "default",
            WriteScope::GlobalOnly,
            fixed(&["info", "debug"]),
        ),
        on(
            Advanced,
            "Hub port",
            "hub.port",
            Shown::Unset,
            "default",
            ANY,
            Number,
        ),
        on(
            Sessions,
            "Compaction tokens",
            "compaction.tokens",
            value("400000"),
            "default",
            ANY,
            Number,
        ),
        on(
            Models,
            "Live model details",
            "model_lists.models_dev",
            value("true"),
            "default",
            ANY,
            Boolean,
        ),
        on(
            Models,
            "Model list refresh",
            "model_lists.refresh_after",
            value("\"24h\""),
            "default",
            ANY,
            Text,
        ),
        on(
            Models,
            "Default model",
            "model",
            value("\"a/b\""),
            "global",
            ANY,
            Model,
        ),
        on(
            Permissions,
            "Reviewer model",
            "reviewer.model",
            Shown::Unset,
            "default",
            WriteScope::GlobalOnly,
            Model,
        ),
        on(
            Permissions,
            "Sandbox",
            "sandbox.enabled",
            value("true"),
            "repository",
            ANY,
            Boolean,
        ),
        on(
            Permissions,
            "Sandbox writable paths",
            "sandbox.allow_write",
            Shown::Unset,
            "default",
            ANY,
            StringList,
        ),
        on(
            Sessions,
            "Repository extensions",
            "repository_extensions",
            Shown::Unset,
            "default",
            WriteScope::RepoOnly,
            RepositoryExtensions,
        ),
        on(
            Models,
            "Scoped models",
            "scoped_models",
            value("[\"a/b\",\"c/d\"]"),
            "global",
            ANY,
            StringList,
        ),
        on(
            Models,
            "Thinking level",
            "thinking",
            Shown::Unset,
            "default",
            ANY,
            fixed(&["off", "minimal", "low", "medium", "high"]),
        ),
        on(
            Notifications,
            "Terminal bell",
            "tui.attention.bell",
            value("false"),
            "default",
            PERSON,
            Boolean,
        ),
        on(
            Appearance,
            "Hover",
            "tui.hover",
            value("true"),
            "default",
            PERSON,
            Boolean,
        ),
        on(
            Appearance,
            "Rail width",
            "tui.rail.width",
            value("30"),
            "project",
            PERSON,
            Number,
        ),
        on(
            Appearance,
            "Theme",
            "tui.theme",
            Shown::Unset,
            "default",
            PERSON,
            Theme,
        ),
        // A key a screen row covers and a `*` instance have no tab.
        row(
            "skills.disabled",
            Shown::Union {
                names: vec![("a".to_owned(), "global".to_owned())],
                own: vec![(Layer::Global, "[\"a\"]".to_owned())],
            },
            "global",
            ANY,
        ),
        row("roles.fast", value("\"a/b\""), "global", ANY),
    ]
}

/// A call's context over `fake`, 24 rows tall, in `/w`.
fn ctx(fake: &Fake) -> Ctx<'_> {
    sized(fake, 80, 24)
}

fn sized(fake: &Fake, width: usize, height: usize) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height,
        width,
        usage: None,
    }
}

/// The view over `fake`, opened with no `-c` overrides.
fn open(fake: &Fake) -> Settings {
    Settings::open(&ctx(fake), Vec::new())
}

/// The view over `fake` on `tab`.
fn focus_non_numeric(settings: &mut Settings, fake: &Fake) -> bool {
    let item = settings.items().into_iter().find(|item| match item {
        super::Item::Key(at) => {
            let row = &settings.rows[*at];
            number_slider(&row.key).is_none()
                && number_list(&row.key).is_none()
                && text_list(&row.key).is_none()
        }
        super::Item::Screen(_) => true,
    });
    let Some(item) = item else { return false };
    settings.focus = settings.focus_of(item);
    settings.place(&ctx(fake));
    true
}

fn tab(fake: &Fake, tab: SettingsTab) -> Settings {
    let mut settings = open(fake);
    while settings.tab != tab {
        assert!(
            focus_non_numeric(&mut settings, fake),
            "{} has only numeric rows",
            super::tab_name(settings.tab)
        );
        settings.edit_key(Edit::Right, &ctx(fake));
    }
    settings
}

#[test]
fn every_tab_has_a_non_numeric_row_for_tab_navigation() {
    let fake = Fake::new(rows());
    for tab_ in SettingsTab::ALL {
        let mut settings = tab(&fake, tab_);
        assert!(
            focus_non_numeric(&mut settings, &fake),
            "{} has only numeric rows",
            super::tab_name(tab_)
        );
    }
}

/// The view over `fake` with the row for `key` focused, on its tab.
fn at(fake: &Fake, key: &str) -> Settings {
    let tab_of = fake
        .rows
        .lock()
        .ok()
        .and_then(|rows| {
            rows.as_ref().ok().and_then(|rows| {
                rows.iter()
                    .find(|row| row.key == key)
                    .and_then(|row| row.presentation.tab)
            })
        })
        .unwrap_or_else(|| panic!("no tabbed row {key}"));
    let mut settings = open(fake);
    settings.tab = tab_of;
    settings.focus = Some(super::Focus::Key(key.to_owned()));
    settings.place(&ctx(fake));
    settings
}

/// Presses each key in turn.
fn press(settings: &mut Settings, fake: &Fake, keys: &[Key]) -> Vec<Act> {
    keys.iter()
        .map(|key| settings.key(key, &ctx(fake)))
        .collect()
}

/// Presses one key and returns what it asked.
fn one(settings: &mut Settings, fake: &Fake, key: Key) -> Act {
    settings.key(&key, &ctx(fake))
}

/// An editing key.
fn edit(settings: &mut Settings, fake: &Fake, edit: Edit) -> Act {
    settings.edit_key(edit, &ctx(fake))
}

/// Types `text` into the open field.
fn typed(settings: &mut Settings, fake: &Fake, text: &str) {
    let keys: Vec<Key> = text.chars().map(Key::Char).collect();
    press(settings, fake, &keys);
}

/// What the writes asked, as (layer, key, text).
fn writes(fake: &Fake) -> Vec<(Layer, String, String)> {
    fake.writes()
        .into_iter()
        .map(|(_, layer, key, text)| (layer, key, text))
        .collect()
}

/// What the resets asked, as (layer, key, expected).
fn resets(fake: &Fake) -> Vec<(Layer, String, String)> {
    support::lock(&fake.resets)
        .iter()
        .map(|(_, layer, key, text)| (*layer, key.clone(), text.clone()))
        .collect()
}

/// `settings` drawn at `width` x `height`, one string per row.
fn drawn(settings: &Settings, usage: Option<u64>, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(
        usize::from(width),
        usize::from(height),
        usage,
        usize::from(width),
    );
    render(&frame, area, &mut buf, &mut Vec::new());
    text_of(&buf, width, height)
}

fn text_of(buf: &Buffer, width: u16, height: u16) -> String {
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The screen at 80x24, no usage.
fn screen(settings: &Settings) -> String {
    drawn(settings, None, 80, 24)
}

/// Compares a drawn screen with its expectation, written inline with a
/// `|` before each row so leading spaces survive. A mismatch shows both.
#[track_caller]
fn expect(screen: &str, want: &str) {
    let want: Vec<&str> = want
        .lines()
        .skip(1)
        .map(|line| line.trim_start().strip_prefix('|').unwrap_or(line))
        .collect();
    let want = want.join("\n");
    assert_eq!(
        screen.trim_end(),
        want.trim_end(),
        "\n--- drawn ---\n{screen}\n--- wanted ---\n{want}\n"
    );
}

/// The line of `screen` for the row labelled `label`, its gutter kept and
/// every run of spaces made one.
fn row_of(screen: &str, label: &str) -> String {
    let line = screen
        .lines()
        .find(|line| {
            line.trim_start_matches(['›', ' '])
                .starts_with(&format!("{label}  "))
        })
        .unwrap_or_else(|| panic!("no row {label} in {screen}"));
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

// The frame: tabs, rows and chrome.

#[test]
fn the_view_opens_on_appearance_with_the_autosave_line_and_tab_bar() {
    let fake = Fake::new(rows());
    let settings = open(&fake);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Hover         [x] On
|
|  Rail width    ▕█████░░░▏ 30% of screen width · 24 columns  project
|
|  Theme         Default
|
|  Key bindings  Open /keys ›
|
|
|
|
|
|
|
|
|
|About tui.hover.
|tui.hover · using the default · Applies when the terminal starts again.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn every_tab_draws_its_rows_through_the_frame_renderer() {
    let fake = Fake::new(rows());
    expect(
        &screen(&tab(&fake, SettingsTab::Appearance)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Hover         [x] On
|
|  Rail width    ▕█████░░░▏ 30% of screen width · 24 columns  project
|
|  Theme         Default
|
|  Key bindings  Open /keys ›
|
|
|
|
|
|
|
|
|
|About tui.hover.
|tui.hover · using the default · Applies when the terminal starts again.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    expect(
        &screen(&tab(&fake, SettingsTab::Notifications)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Terminal bell  [ ] Off
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|About tui.attention.bell.
|tui.attention.bell · using the default · Applies when the terminal starts again.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    expect(
        &screen(&tab(&fake, SettingsTab::Models)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Default model       a/b ›
|
|  Live model details  [x] On
|
|  Model list refresh  24 h
|
|  Scoped models       a/b, c/d ›
|
|  Thinking level      Default
|
|  Credentials         Open /credential ›
|
|
|
|
|
|About model.
|model · set in global · Applies to new sessions.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    expect(
        &screen(&tab(&fake, SettingsTab::Permissions)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Reviewer model          Default ›
|
|  Sandbox writable paths  Default ›
|
|  Sandbox                 [x] On  repository
|
|  Rules                   Open /rules ›
|
|  Tools                   Open /tools ›
|
|
|
|
|
|
|
|About reviewer.model.
|reviewer.model · not set · changing reviewer.model needs a reload
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    expect(
        &screen(&tab(&fake, SettingsTab::Sessions)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Compaction tokens      400k tokens
|
|  Repository extensions  Default ›
|
|  Skills                 Open /skills ›
|
|  MCP servers            Open /mcp ›
|
|
|
|
|
|
|
|
|
|About compaction.tokens.
|compaction.tokens · using the default · changing compaction.tokens needs a reloa
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    expect(
        &screen(&tab(&fake, SettingsTab::Advanced)),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Diagnostics level  info
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|About diagnostics.level.
|diagnostics.level · using the default · Applies when each Fiber process starts a
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn the_open_tab_is_bold_on_an_accent_fill() {
    use crate::theme::Role;
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None, 80);
    render(&frame, area, &mut buf, &mut Vec::new());
    let bar = (0..24u16)
        .find(|y| {
            text_of(&buf, 80, 24)
                .lines()
                .nth(usize::from(*y))
                .is_some_and(|l| l.contains("Notifications"))
        })
        .unwrap();
    let cell_at = |text: &str, y: u16| {
        let line: Vec<&str> = (0..80u16).map(|x| buf[(x, y)].symbol()).collect();
        let joined = line.concat();
        let byte = joined.find(text).unwrap();
        let x = u16::try_from(joined[..byte].chars().count()).unwrap();
        buf[(x, y)].clone()
    };
    // The open tab is bold in `background` on an `accent` fill.
    let open = cell_at("Models", bar);
    assert!(open.modifier.contains(Modifier::BOLD));
    assert_eq!(open.fg, Role::Background.color());
    assert_eq!(open.bg, Role::Accent.color());
    // The other tabs are dim, with no accent fill and no bold.
    for name in ["Appearance", "Notifications", "Permissions", "Advanced"] {
        let cell = cell_at(name, bar);
        assert!(cell.modifier.contains(Modifier::DIM), "{name}");
        assert!(!cell.modifier.contains(Modifier::BOLD), "{name}");
        assert_ne!(cell.bg, Role::Accent.color(), "{name}");
    }
    // The footer keys are bold and the words between them are dim.
    let last = 23u16;
    for key in ["Enter", "Tab", "Delete"] {
        assert!(
            cell_at(key, last).modifier.contains(Modifier::BOLD),
            "{key}"
        );
    }
    assert!(cell_at("change", last).modifier.contains(Modifier::DIM));
}

/// The Models tab paints each cell by meaning: the focused label bold
/// in `accent`, the focused value bold in `info`, other values plain
/// in `info`, "Default" dim, the screen row's value in `secondary`,
/// and the gutter mark bold in `accent`.
#[test]
fn a_models_tab_pins_each_cell_style_by_meaning() {
    use crate::theme::Role;
    use ratatui::style::Color;
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None, 80);
    render(&frame, area, &mut buf, &mut Vec::new());
    let row_of = |label: &str| {
        (0..24u16)
            .find(|y| {
                text_of(&buf, 80, 24)
                    .lines()
                    .nth(usize::from(*y))
                    .is_some_and(|l| l.contains(label))
            })
            .unwrap_or_else(|| panic!("no row with {label}"))
    };
    let cell_at = |x: u16, y: u16| buf[(x, y)].clone();
    // The focused row carries the gutter mark, bold in `accent`.
    let focused = row_of("› Default model");
    assert_eq!(cell_at(0, focused).symbol(), "›");
    assert_eq!(cell_at(0, focused).fg, Role::Accent.color());
    assert!(
        cell_at(0, focused).modifier.contains(Modifier::BOLD),
        "gutter mark not bold"
    );
    // Its label is bold in `accent`; other labels are plain.
    let label = cell_at(2, focused);
    assert_eq!(label.fg, Role::Accent.color());
    assert!(label.modifier.contains(Modifier::BOLD), "label not bold");
    let plain_row = row_of("Model list refresh");
    let plain_label = cell_at(2, plain_row);
    assert_eq!(plain_label.fg, Color::Reset, "unfocused label tinted");
    assert!(
        !plain_label.modifier.contains(Modifier::BOLD),
        "unfocused label bold"
    );
    // Its value is bold in `info`; other values are plain `info`.
    let line_of = |y: u16| {
        let line: String = (0..80u16).map(|x| buf[(x, y)].symbol()).collect();
        line
    };
    let at_in = |y: u16, text: &str| {
        u16::try_from(line_of(y).find(text).unwrap_or_else(|| panic!("no {text}"))).unwrap()
    };
    let value = cell_at(at_in(focused, "a/b"), focused);
    assert_eq!(value.fg, Role::Info.color());
    assert!(value.modifier.contains(Modifier::BOLD), "value not bold");
    let plain_value = cell_at(at_in(plain_row, "24 h"), plain_row);
    assert_eq!(plain_value.fg, Role::Info.color());
    assert!(
        !plain_value.modifier.contains(Modifier::BOLD),
        "unfocused value bold"
    );
    // An unset value reads dim.
    let default_row = row_of("Thinking level");
    let default = cell_at(at_in(default_row, "Default"), default_row);
    assert_eq!(default.fg, Role::Muted.color());
    assert!(default.modifier.contains(Modifier::DIM), "Default not dim");
    assert!(!default.modifier.contains(Modifier::BOLD), "Default bold");
    // A screen row's value is in `secondary`.
    let screen_row = row_of("Credentials");
    let screen_value = cell_at(at_in(screen_row, "Open"), screen_row);
    assert_eq!(screen_value.fg, Role::Secondary.color());
}

/// A layer tag reads in `attention`, `[x] On` in `added`, and `[ ]
/// Off` dim, whatever the focus.
#[test]
fn a_layer_tag_and_a_boolean_keep_their_roles() {
    use crate::theme::Role;
    let fake = Fake::new(rows());
    let settings = at(&fake, "sandbox.enabled");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None, 80);
    render(&frame, area, &mut buf, &mut Vec::new());
    let row_of = |label: &str| {
        (0..24u16)
            .find(|y| {
                text_of(&buf, 80, 24)
                    .lines()
                    .nth(usize::from(*y))
                    .is_some_and(|l| l.contains(label))
            })
            .unwrap_or_else(|| panic!("no row with {label}"))
    };
    let row = row_of("› Sandbox");
    let line: String = (0..80u16).map(|x| buf[(x, row)].symbol()).collect();
    let on_at = u16::try_from(line.find("[x] On").unwrap()).unwrap();
    let on = buf[(on_at, row)].clone();
    assert_eq!(on.fg, Role::Added.color());
    let tag_at = u16::try_from(line.find("repository").unwrap()).unwrap();
    let tag = buf[(tag_at, row)].clone();
    assert_eq!(tag.fg, Role::Attention.color());
    // `[ ] Off` stays dim on its own row.
    let fake = Fake::new(rows());
    let settings = at(&fake, "tui.attention.bell");
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None, 80);
    render(&frame, area, &mut buf, &mut Vec::new());
    let row = (0..24u16)
        .find(|y| {
            text_of(&buf, 80, 24)
                .lines()
                .nth(usize::from(*y))
                .is_some_and(|l| l.contains("› Terminal bell"))
        })
        .unwrap();
    let line: String = (0..80u16).map(|x| buf[(x, row)].symbol()).collect();
    let off_at = u16::try_from(line.find("[ ] Off").unwrap()).unwrap();
    let off = buf[(off_at, row)].clone();
    assert!(off.modifier.contains(Modifier::DIM), "[ ] Off not dim");
    assert!(!off.modifier.contains(Modifier::BOLD), "[ ] Off bold");
}

/// ←→ moves between tabs and wraps from the last tab to the first
/// and back (`docs/tui.md:1108-1109`, `/settings`).
#[test]
fn left_and_right_wrap_between_the_first_and_last_tabs() {
    let fake = Fake::new(rows());
    let mut settings = open(&fake);
    // Left on the first tab wraps to the last.
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(settings.tab, SettingsTab::Advanced);
    // Right on the last tab wraps to the first.
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    // Between tabs it still steps one at a time.
    for want in &SettingsTab::ALL[1..] {
        assert!(focus_non_numeric(&mut settings, &fake));
        edit(&mut settings, &fake, Edit::Right);
        assert_eq!(settings.tab, *want);
    }
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(settings.tab, SettingsTab::Sessions);
}

#[test]
fn a_key_with_no_tab_has_no_row_and_every_tabbed_key_has_exactly_one() {
    let fake = Fake::new(rows());
    let mut drawn_keys = Vec::new();
    for tab_ in SettingsTab::ALL {
        let settings = tab(&fake, tab_);
        for item in settings.items() {
            if let super::Item::Key(at) = item {
                drawn_keys.push(settings.rows[at].key.clone());
            }
        }
    }
    let mut want: Vec<String> = rows()
        .into_iter()
        .filter(|row| row.presentation.tab.is_some())
        .map(|row| row.key)
        .collect();
    drawn_keys.sort();
    want.sort();
    assert!(drawn_keys.iter().all(|key| key != "hub.port"));
    want.retain(|key| key != "hub.port");
    assert_eq!(drawn_keys, want);
    assert!(
        !drawn_keys
            .iter()
            .any(|key| key == "skills.disabled" || key == "roles.fast")
    );
}

#[test]
fn screen_rows_end_their_tab_in_the_documented_order() {
    let fake = Fake::new(rows());
    let ends = |tab_: SettingsTab| -> Vec<&'static str> {
        tab(&fake, tab_)
            .items()
            .into_iter()
            .filter_map(|item| match item {
                super::Item::Screen(screen) => screen.row().map(|row| row.metadata.label),
                super::Item::Key(_) => None,
            })
            .collect()
    };
    assert_eq!(ends(SettingsTab::Appearance), ["Key bindings"]);
    assert_eq!(ends(SettingsTab::Models), ["Credentials"]);
    assert_eq!(ends(SettingsTab::Permissions), ["Rules", "Tools"]);
    assert_eq!(ends(SettingsTab::Sessions), ["Skills", "MCP servers"]);
    assert!(ends(SettingsTab::Notifications).is_empty());
    assert!(ends(SettingsTab::Advanced).is_empty());
    // A screen row comes after every key of its tab.
    for tab_ in SettingsTab::ALL {
        let items = tab(&fake, tab_).items();
        let first_screen = items
            .iter()
            .position(|item| matches!(item, super::Item::Screen(_)));
        if let Some(first) = first_screen {
            assert!(
                items[first..]
                    .iter()
                    .all(|item| matches!(item, super::Item::Screen(_)))
            );
        }
    }
}

#[test]
fn permissions_lists_the_reviewer_keys_before_the_sandbox_keys() {
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Permissions);
    let keys: Vec<&str> = settings
        .items()
        .into_iter()
        .filter_map(|item| match item {
            super::Item::Key(at) => settings.rows.get(at).map(|row| row.key.as_str()),
            super::Item::Screen(_) => None,
        })
        .collect();
    assert_eq!(
        keys,
        ["reviewer.model", "sandbox.allow_write", "sandbox.enabled"]
    );
}

#[test]
fn an_empty_tab_says_so_and_takes_no_focus() {
    let fake = Fake::new(Vec::new());
    let mut settings = tab(&fake, SettingsTab::Notifications);
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|  Nothing is set on this tab.
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    assert!(matches!(one(&mut settings, &fake, Key::Down), Act::Stay));
    assert!(matches!(one(&mut settings, &fake, Key::Enter), Act::Stay));
    assert!(fake.writes().is_empty());
}

#[test]
fn a_failed_read_says_why_and_shows_no_rows() {
    let fake = Fake::new(rows());
    *support::lock(&fake.rows) = Err(crate::configure::ConfigureError {
        code: contract::ErrorCode::IoFailed,
        message: "no config".to_owned(),
    });
    let settings = open(&fake);
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Key bindings  Open /keys ›
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|Rebind the terminal's keys, one action at a time.
|/keys · opens its own screen
|no config
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn rows_space_one_blank_line_apart_only_when_the_frame_is_tall_enough() {
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    // Models has 5 keys and one screen row: 6 rows need 11 body lines.
    let tall = drawn(&settings, None, 80, 24);
    let lines: Vec<&str> = tall.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("Default model"))
        .unwrap();
    assert_eq!(lines.get(at + 1), Some(&""), "{tall}");
    // Below 18 rows the top rows drop their blank lines: two fixed rows,
    // two lines under the list. A body of exactly nine lines is spaced.
    let at_edge = settings.layout(80, 2 + 2 + 2 + 11, None);
    assert_eq!(at_edge.body, 11);
    assert!(at_edge.lines.contains(&super::Line::Blank));
    // One line fewer and the rows pack.
    let packed = settings.layout(80, 2 + 2 + 2 + 10, None);
    assert_eq!(packed.body, 10);
    assert!(!packed.lines.contains(&super::Line::Blank));
    assert_eq!(packed.lines.len(), 6);
}

#[test]
fn a_short_frame_drops_the_description_before_the_controls() {
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    // Compact top rows (no blank lines) and a small body.
    let layout = settings.layout(80, 10, None);
    assert_eq!(
        layout.top.len(),
        2,
        "no blank lines in the top rows below 18"
    );
    assert!(layout.body >= super::MIN_BODY, "body {}", layout.body);
    assert_eq!(layout.below.len(), 10 - 2 - 2 - layout.body);
    // Every row of the tab can still be reached by the keys, whatever
    // was dropped.
    let screen = drawn(&settings, None, 80, 10);
    assert!(screen.contains("Model list refresh"), "{screen}");
    assert!(screen.lines().count() <= 10);
}

#[test]
fn the_focused_row_stays_in_view_when_the_frame_shrinks() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "scoped_models");
    for height in [24usize, 14, 10, 8, 6, 5, 4, 3, 2, 1] {
        let screen = drawn(&settings, None, 80, u16::try_from(height).unwrap());
        assert!(screen.lines().count() <= height, "{height}");
        if height >= 8 {
            assert!(
                screen.contains("› Scoped models"),
                "height {height}:\n{screen}"
            );
        }
    }
    // A key at the new size keeps working.
    one(&mut settings, &fake, Key::Down);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("thinking".to_owned()))
    );
}

#[test]
fn a_narrow_frame_scrolls_the_tab_bar_to_keep_the_open_tab() {
    let fake = Fake::new(rows());
    // The full bar is 73 columns wide: it fits at 73, not at 72.
    let settings = tab(&fake, SettingsTab::Advanced);
    let widest = settings.tab_bar(73);
    assert_eq!(
        widest.iter().map(|(t, _)| t.chars().count()).sum::<usize>(),
        73
    );
    assert!(widest.iter().any(|(t, _)| t.contains("Appearance")));
    let narrower = settings.tab_bar(72);
    assert!(
        narrower
            .iter()
            .map(|(t, _)| t.chars().count())
            .sum::<usize>()
            <= 72
    );
    assert!(!narrower.iter().any(|(t, _)| t.contains("Appearance")));
    assert!(narrower.iter().any(|(t, _)| t.contains("Advanced")));
    let tiny = settings.tab_bar(5);
    assert!(tiny.iter().any(|(t, _)| t.contains("Advanced")));
    // Drawing never panics at any width.
    for width in 1..=100u16 {
        drawn(&settings, None, width, 12);
    }
}

// Controls.

/// The fixture rows with `key` holding `shown` from `layer`.
fn editor_fake() -> Fake {
    Fake::new(vec![on(
        SettingsTab::Appearance,
        "Editor",
        "tui.editor",
        value("\"24h\""),
        "global",
        ANY,
        SettingControl::Text,
    )])
}

fn with(key: &str, shown: Shown, layer: &str) -> Vec<SettingRow> {
    rows()
        .into_iter()
        .map(|mut row| {
            if row.key == key {
                row.value = shown.clone();
                row.layer = layer.to_owned();
                row.file = match layer {
                    "global" => Some(file(Layer::Global)),
                    "project" => Some(file(Layer::Project)),
                    "repository" => Some(file(Layer::Repository)),
                    _ => None,
                };
            }
            row
        })
        .collect()
}

fn refuse(fake: &Fake, message: &str) {
    *support::lock(&fake.refuse) = Some(message.to_owned());
}

fn w(layer: Layer, key: &str, text: &str) -> (Layer, String, String) {
    (layer, key.to_owned(), text.to_owned())
}

#[test]
fn a_boolean_shows_on_or_off_and_enter_and_space_flip_it_in_its_own_layer() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "sandbox.enabled");
    assert_eq!(
        row_of(&screen(&settings), "Sandbox"),
        "› Sandbox [x] On repository"
    );
    // Enter turns it off, in the repository file it comes from.
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Repository, "sandbox.enabled", "false")]
    );
    assert_eq!(settings.said, ["Saved to repository config."]);
    // Space turns an off value on again.
    *support::lock(&fake.rows) = Ok(with("sandbox.enabled", value("false"), "repository"));
    settings.reread(&ctx(&fake));
    assert_eq!(
        row_of(&screen(&settings), "Sandbox"),
        "› Sandbox [ ] Off repository"
    );
    one(&mut settings, &fake, Key::Char(' '));
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Repository, "sandbox.enabled", "true"))
    );
}

#[test]
fn a_boolean_no_layer_sets_saves_to_global_and_an_unset_one_turns_on() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.hover");
    // A default shows no layer tag, and the write goes to global.
    assert_eq!(row_of(&screen(&settings), "Hover"), "› Hover [x] On");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.hover", "false")]);
    // A boolean with no value at all reads Default and turns on.
    *support::lock(&fake.rows) = Ok(with("tui.hover", Shown::Unset, "default"));
    settings.reread(&ctx(&fake));
    assert_eq!(row_of(&screen(&settings), "Hover"), "› Hover Default");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "tui.hover", "true"))
    );
}

#[test]
fn a_fixed_set_steps_in_the_schemas_order_and_wraps_without_passing_unset() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "thinking");
    // Unset reads Default with no layer tag.
    assert_eq!(
        row_of(&screen(&settings), "Thinking level"),
        "› Thinking level Default"
    );
    // The first Enter writes the first value, global (no layer sets it).
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "thinking", "\"off\"")]);
    // The first Space does the same.
    let mut again = at(&fake, "thinking");
    one(&mut again, &fake, Key::Char(' '));
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "thinking", "\"off\""))
    );
    // Every step moves one place, in the order given (not sorted: `high`
    // is not before `low`), and the last wraps to the first.
    let order = ["off", "minimal", "low", "medium", "high"];
    for (at_, current) in order.iter().enumerate() {
        *support::lock(&fake.rows) = Ok(with(
            "thinking",
            value(&format!("\"{current}\"")),
            "project",
        ));
        let mut step = at(&fake, "thinking");
        one(&mut step, &fake, Key::Enter);
        let next = order[(at_ + 1) % order.len()];
        assert_eq!(
            writes(&fake).last(),
            Some(&w(Layer::Project, "thinking", &format!("\"{next}\""))),
            "after {current}"
        );
    }
    // A value outside the set (a stale file) starts again at the first.
    *support::lock(&fake.rows) = Ok(with("thinking", value("\"bogus\""), "global"));
    let mut off = at(&fake, "thinking");
    one(&mut off, &fake, Key::Enter);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "thinking", "\"off\""))
    );
}

#[test]
fn a_default_valued_fixed_key_steps_from_the_default() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "diagnostics.level");
    assert_eq!(
        row_of(&screen(&settings), "Diagnostics level"),
        "› Diagnostics level info"
    );
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "diagnostics.level", "\"debug\"")]
    );
}

#[test]
fn a_refused_step_changes_nothing_on_screen() {
    let fake = Fake::new(rows());
    refuse(&fake, "refused");
    let mut settings = at(&fake, "thinking");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(settings.said, ["refused"]);
    assert_eq!(
        row_of(&screen(&settings), "Thinking level"),
        "› Thinking level Default"
    );
}

/// A seam whose `themes/` holds `dusk` and `solar`.
fn themed(rows: Vec<SettingRow>) -> Fake {
    let mut fake = Fake::new(rows);
    fake.themes = vec!["dusk".to_owned(), "solar".to_owned()];
    fake
}

#[test]
fn the_theme_steps_through_the_built_ins_then_the_files_and_applies_after_it_saved() {
    let fake = themed(rows());
    let mut settings = at(&fake, "tui.theme");
    // Unset: Default, and the first step writes the first theme.
    assert_eq!(row_of(&screen(&settings), "Theme"), "› Theme Default");
    let act = one(&mut settings, &fake, Key::Enter);
    assert!(matches!(act, Act::Theme(ThemeSetting::Follow)), "{act:?}");
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.theme", "\"auto\"")]);
    let order = ["auto", "dark", "light", "dusk", "solar"];
    for (at_, current) in order.iter().enumerate() {
        *support::lock(&fake.rows) = Ok(with(
            "tui.theme",
            value(&format!("\"{current}\"")),
            "global",
        ));
        let mut step = at(&fake, "tui.theme");
        let act = one(&mut step, &fake, Key::Enter);
        let next = order[(at_ + 1) % order.len()];
        assert_eq!(
            writes(&fake).last(),
            Some(&w(Layer::Global, "tui.theme", &format!("\"{next}\"")))
        );
        assert!(matches!(act, Act::Theme(_)), "{act:?}");
    }
    // From the last theme the steps wrap to the first.
    let act = one(&mut at(&fake, "tui.theme"), &fake, Key::Enter);
    assert!(matches!(&act, Act::Theme(ThemeSetting::Follow)), "{act:?}");
}

#[test]
fn a_project_theme_saves_to_the_project_not_global() {
    let fake = themed(with("tui.theme", value("\"dark\""), "project"));
    let mut settings = at(&fake, "tui.theme");
    let act = one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.theme", "\"light\"")]);
    assert!(matches!(act, Act::Theme(ThemeSetting::Light)), "{act:?}");
}

#[test]
fn a_refused_theme_step_applies_nothing() {
    let fake = themed(rows());
    refuse(&fake, "refused");
    let mut settings = at(&fake, "tui.theme");
    let act = one(&mut settings, &fake, Key::Enter);
    assert!(matches!(act, Act::Stay), "{act:?}");
    assert_eq!(settings.said, ["refused"]);
}

#[test]
fn deleting_a_theme_resets_it_and_applies_the_inherited_one() {
    let fake = themed(with("tui.theme", value("\"light\""), "project"));
    // Once the project's value is gone the global one shows.
    *support::lock(&fake.rows_after) = Some(with("tui.theme", value("\"dark\""), "global"));
    let mut settings = at(&fake, "tui.theme");
    let act = edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(resets(&fake), [w(Layer::Project, "tui.theme", "\"light\"")]);
    assert!(matches!(act, Act::Theme(ThemeSetting::Dark)), "{act:?}");
    assert_eq!(settings.said, ["Removed tui.theme from project config."]);
    // With nothing below, the theme follows the terminal again.
    let fake = themed(with("tui.theme", value("\"light\""), "global"));
    *support::lock(&fake.rows_after) = Some(with("tui.theme", Shown::Unset, "default"));
    let act = edit(&mut at(&fake, "tui.theme"), &fake, Edit::Delete);
    assert!(matches!(act, Act::Theme(ThemeSetting::Follow)), "{act:?}");
}

#[test]
fn a_bounded_number_steps_instead_of_opening_a_text_field() {
    let fake = Fake::new(with("tui.rail.width", value("30"), "project"));
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "31")]);
    assert_eq!(settings.said, ["Saved to project config."]);
}

#[test]
fn a_saved_write_rereads_and_says_the_file() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "compaction.tokens");
    // Tab moves the default to project before Enter steps the list.
    *support::lock(&fake.rows_after) = Some(with("compaction.tokens", value("400000"), "project"));
    press(&mut settings, &fake, &[Key::Tab, Key::Enter]);
    let reads = fake.reads().len();
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Project, "compaction.tokens", "600000"))
    );
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(fake.reads().len(), reads + 1);
    assert_eq!(settings.said, ["Saved to project config."]);
    let frame = settings.frame(80, 24, None, 80);
    assert!(frame.below.iter().any(|line| {
        line.contains("compaction.tokens")
            && line.contains("changing compaction.tokens needs a reload")
    }));
}

#[test]
fn free_text_is_saved_as_a_string_whatever_it_looks_like() {
    let fake = editor_fake();
    let mut settings = at(&fake, "tui.editor");
    one(&mut settings, &fake, Key::Enter);
    press(
        &mut settings,
        &fake,
        &[Key::Backspace, Key::Backspace, Key::Backspace],
    );
    typed(&mut settings, &fake, "7d");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.editor", "\"7d\"")]);
    // Text that parses as JSON is still text.
    let mut again = at(&fake, "tui.editor");
    one(&mut again, &fake, Key::Enter);
    press(
        &mut again,
        &fake,
        &[Key::Backspace, Key::Backspace, Key::Backspace],
    );
    typed(&mut again, &fake, "12");
    one(&mut again, &fake, Key::Enter);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "tui.editor", "\"12\""))
    );
}

#[test]
fn a_refused_numeric_step_keeps_the_value_and_reports_the_error() {
    let fake = Fake::new(with("tui.rail.width", value("30"), "project"));
    refuse(&fake, "tui.rail.width must be a number");
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "31")]);
    assert_eq!(settings.said, ["tui.rail.width must be a number"]);
    one(&mut settings, &fake, Key::Esc);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(writes(&fake).len(), 1);
}

#[test]
fn esc_in_a_field_cancels_with_no_write() {
    let fake = editor_fake();
    let mut settings = at(&fake, "tui.editor");
    press(
        &mut settings,
        &fake,
        &[Key::Enter, Key::Char('9'), Key::Esc],
    );
    assert!(fake.writes().is_empty());
    assert!(matches!(settings.mode, super::Mode::Rows));
    // A second Esc leaves the view: Esc in the rows closes it.
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn the_field_takes_paste_and_moves_its_caret_by_characters() {
    let fake = editor_fake();
    let mut settings = at(&fake, "tui.editor");
    one(&mut settings, &fake, Key::Enter);
    press(
        &mut settings,
        &fake,
        &[Key::Backspace, Key::Backspace, Key::Backspace],
    );
    edit(&mut settings, &fake, Edit::Paste("a\tb\nc".to_owned()));
    typed(&mut settings, &fake, "é日");
    edit(&mut settings, &fake, Edit::Left);
    edit(&mut settings, &fake, Edit::Left);
    typed(&mut settings, &fake, "x");
    let super::Mode::Field(field) = &settings.mode else {
        panic!("no field");
    };
    assert_eq!(field.draft.expand(), "abcxé日");
    assert_eq!(field.draft.position(), 4);
    // ← → and Delete edit the draft: they neither change the tab nor
    // reset the value.
    edit(&mut settings, &fake, Edit::Delete);
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert!(resets(&fake).is_empty());
    let super::Mode::Field(field) = &settings.mode else {
        panic!("no field");
    };
    assert_eq!(field.draft.expand(), "abcx日");
}

#[test]
fn the_wheel_and_clicks_do_not_retarget_an_open_field() {
    let fake = editor_fake();
    let mut settings = at(&fake, "tui.editor");
    one(&mut settings, &fake, Key::Enter);
    settings.wheel(false, &ctx(&fake));
    settings.click(Spot::Row(0), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.editor".to_owned()))
    );
    assert!(matches!(settings.mode, super::Mode::Field(_)));
}

#[test]
fn a_model_row_ends_in_an_arrow_and_asks_for_the_picker_with_its_key_and_layer() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model");
    assert_eq!(
        row_of(&screen(&settings), "Default model"),
        "› Default model a/b ›"
    );
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(settings.take_request(), Some(Request::Model));
    assert_eq!(settings.awaiting, Some(("model".to_owned(), Layer::Global)));
    assert!(settings.awaiting_model());
    // Nothing is written until the picker chooses.
    assert!(fake.writes().is_empty());
    // Space asks the same, on a key whose value is unset.
    let mut reviewer = at(&fake, "reviewer.model");
    one(&mut reviewer, &fake, Key::Char(' '));
    assert_eq!(reviewer.take_request(), Some(Request::Model));
    assert_eq!(
        reviewer.awaiting,
        Some(("reviewer.model".to_owned(), Layer::Global))
    );
    assert!(settings.take_request().is_none());
}

#[test]
fn the_pickers_choice_saves_the_reference_byte_for_byte_to_the_rows_key() {
    let fake = Fake::new(with("model", value("\"a/b\""), "project"));
    let mut settings = at(&fake, "model");
    one(&mut settings, &fake, Key::Enter);
    let reference = "acme/m.1@work:high \"q\"";
    assert!(settings.choose_model(reference, &ctx(&fake)));
    assert_eq!(
        writes(&fake),
        [w(
            Layer::Project,
            "model",
            &serde_json::Value::String(reference.to_owned()).to_string()
        )]
    );
    assert!(!settings.awaiting_model());
    // A choice with nothing waiting writes nothing.
    assert!(!settings.choose_model("x/y", &ctx(&fake)));
    assert_eq!(writes(&fake).len(), 1);
}

#[test]
fn a_refused_model_save_keeps_the_target_for_a_retry_and_a_cancel_clears_it() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "reviewer.model");
    one(&mut settings, &fake, Key::Enter);
    refuse(&fake, "refused");
    assert!(!settings.choose_model("a/b", &ctx(&fake)));
    assert!(settings.awaiting_model());
    assert_eq!(settings.last_said(), Some("refused"));
    *support::lock(&fake.refuse) = None;
    assert!(settings.choose_model("a/b", &ctx(&fake)));
    assert_eq!(writes(&fake).len(), 2);
    // Closing the picker with no choice writes nothing and forgets it.
    one(&mut settings, &fake, Key::Enter);
    settings.model_cancelled();
    assert!(!settings.awaiting_model());
    assert!(!settings.choose_model("c/d", &ctx(&fake)));
    assert_eq!(writes(&fake).len(), 2);
}

#[test]
fn each_screen_row_asks_for_its_screen_and_writes_no_key() {
    let fake = Fake::new(rows());
    for screen_ in Screen::ALL {
        let row_tab = screen_.row().and_then(|row| row.metadata.tab).unwrap();
        for key in [Key::Enter, Key::Char(' ')] {
            let mut settings = tab(&fake, row_tab);
            settings.focus = Some(super::Focus::Screen(screen_));
            settings.place(&ctx(&fake));
            one(&mut settings, &fake, key);
            assert_eq!(settings.take_request(), Some(Request::Screen(screen_)));
            assert!(!settings.awaiting_model());
        }
    }
    assert!(fake.writes().is_empty());
    assert!(resets(&fake).is_empty());
}

#[test]
fn the_screen_table_and_the_screens_agree() {
    let commands: Vec<&str> = Screen::ALL
        .iter()
        .map(|screen| screen.row().map_or("", |row| row.command))
        .collect();
    assert_eq!(
        commands,
        [
            "/keys",
            "/credential",
            "/rules",
            "/tools",
            "/skills",
            "/mcp"
        ]
    );
    assert_eq!(Screen::ALL.len(), SCREEN_ROWS.len());
}

#[test]
fn a_screen_rows_value_reads_open_with_its_command() {
    let fake = Fake::new(rows());
    let mut settings = tab(&fake, SettingsTab::Appearance);
    settings.focus = Some(super::Focus::Screen(Screen::Keys));
    settings.place(&ctx(&fake));
    assert_eq!(
        row_of(&screen(&settings), "Key bindings"),
        "› Key bindings Open /keys ›"
    );
}

// Where a change saves, read-only values, moving and resetting.

#[test]
fn a_value_set_by_dash_c_is_read_only_whatever_opens_it() {
    // One row per control, each set by `-c`.
    for key in [
        "tui.hover",
        "thinking",
        "tui.theme",
        "tui.rail.width",
        "model_lists.refresh_after",
        "model",
        "scoped_models",
        "repository_extensions",
    ] {
        let value = match key {
            "tui.hover" => "true",
            "thinking" => "\"low\"",
            "tui.theme" => "\"dark\"",
            "tui.rail.width" => "30",
            "model_lists.refresh_after" => "\"1d\"",
            "model" => "\"a/b\"",
            "scoped_models" => "[\"a/b\"]",
            _ => "[{\"path\":\"p\",\"required\":true}]",
        };
        let fake = Fake::new(with(key, super::Shown::Value(value.to_owned()), "-c"));
        let mut settings = at(&fake, key);
        let shown = screen(&settings);
        let row = shown.lines().find(|line| line.starts_with('›')).unwrap();
        assert!(row.ends_with("  -c"), "{key}: {row}");
        assert!(shown.contains("set by -c for this run"), "{key}:\n{shown}");
        for press_ in [Key::Enter, Key::Char(' '), Key::Tab] {
            let act = one(&mut settings, &fake, press_.clone());
            assert!(matches!(act, Act::Stay), "{key} {press_:?}");
        }
        if matches!(key, "tui.rail.width" | "model_lists.refresh_after") {
            for arrow in [Edit::Left, Edit::Right] {
                let act = edit(&mut settings, &fake, arrow.clone());
                assert!(matches!(act, Act::Stay), "{key} {arrow:?}");
            }
        }
        let act = edit(&mut settings, &fake, Edit::Delete);
        assert!(matches!(act, Act::Stay), "{key}");
        assert!(fake.writes().is_empty(), "{key}: wrote {:?}", fake.writes());
        assert!(resets(&fake).is_empty(), "{key}");
        assert!(settings.take_request().is_none(), "{key}");
        assert!(!settings.awaiting_model(), "{key}");
        assert!(settings.page.is_none(), "{key}");
        assert!(matches!(settings.mode, super::Mode::Rows), "{key}");
    }
}

#[test]
fn a_theme_set_by_dash_c_opens_no_choices_under_slash_theme() {
    let fake = Fake::new(with(
        "tui.theme",
        super::Shown::Value("\"dark\"".to_owned()),
        "-c",
    ));
    let mut settings = Settings::open_theme(&ctx(&fake), vec!["tui.theme=dark".to_owned()]);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert!(settings.choices_entry().is_none());
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.theme".to_owned()))
    );
    assert!(screen(&settings).contains("set by -c for this run"));
    for press_ in [Key::Enter, Key::Char(' '), Key::Tab] {
        let act = one(&mut settings, &fake, press_.clone());
        assert!(!matches!(act, Act::Theme(_)), "{press_:?}");
    }
    assert!(fake.writes().is_empty(), "wrote {:?}", fake.writes());
    assert!(resets(&fake).is_empty());
}

#[test]
fn the_run_overrides_reach_every_read_of_the_rows() {
    let fake = Fake::new(rows());
    let overrides = vec!["thinking=high".to_owned(), "thinking=low".to_owned()];
    let mut settings = Settings::open(&ctx(&fake), overrides.clone());
    one(&mut settings, &fake, Key::Down);
    settings.reread(&ctx(&fake));
    let queries = support::lock(&fake.settings_queries).clone();
    assert_eq!(queries.len(), 2);
    assert!(
        queries
            .iter()
            .all(|(workspace, got)| workspace == Path::new("/w") && *got == overrides)
    );
}

#[test]
fn a_save_reads_the_rows_again_with_the_same_overrides() {
    let fake = Fake::new(rows());
    let overrides = vec!["thinking=low".to_owned()];
    let mut settings = Settings::open(&ctx(&fake), overrides.clone());
    settings.focus = Some(super::Focus::Key("tui.hover".to_owned()));
    one(&mut settings, &fake, Key::Enter);
    let queries = support::lock(&fake.settings_queries).clone();
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[1].1, overrides);
}

/// `Tab` on `key`, whose value is `value` from `layer` within `scope`.
fn tabbed(key: &str, shown: Shown, layer: &str, scope: WriteScope) -> (Fake, Settings) {
    let mut all = with(key, shown, layer);
    for row in &mut all {
        if row.key == key {
            row.scope = scope;
        }
    }
    let fake = Fake::new(all);
    let mut settings = at(&fake, key);
    one(&mut settings, &fake, Key::Tab);
    (fake, settings)
}

#[test]
fn tab_moves_a_value_to_the_next_layer_the_key_allows_and_wraps() {
    let any = WriteScope::Any { repo: true };
    let person = WriteScope::PersonFiles;
    let none = WriteScope::Any { repo: false };
    for (scope, from, to) in [
        (any, "global", Layer::Project),
        (any, "project", Layer::Repository),
        (any, "repository", Layer::Global),
        (none, "global", Layer::Project),
        (none, "project", Layer::Global),
        (person, "global", Layer::Project),
        (person, "project", Layer::Global),
    ] {
        let (fake, settings) = tabbed("tui.rail.width", value("30"), from, scope);
        let source = super::layer_named(from).unwrap();
        // The destination is written, then the source is emptied.
        assert_eq!(
            writes(&fake),
            [w(to, "tui.rail.width", "30")],
            "{scope:?} {from}"
        );
        assert_eq!(
            resets(&fake),
            [w(source, "tui.rail.width", "30")],
            "{scope:?} {from}"
        );
        assert_eq!(
            settings.said,
            [format!(
                "Moved from {from} to {} config.",
                super::layer_name(to)
            )]
        );
    }
}

#[test]
fn tab_does_nothing_on_a_key_with_one_layer() {
    for scope in [WriteScope::GlobalOnly, WriteScope::RepoOnly] {
        let layer = if scope == WriteScope::GlobalOnly {
            "global"
        } else {
            "repository"
        };
        let (fake, _) = tabbed("tui.rail.width", value("30"), layer, scope);
        assert!(fake.writes().is_empty(), "{scope:?}");
        assert!(resets(&fake).is_empty(), "{scope:?}");
    }
}

#[test]
fn tab_on_a_default_copies_it_down_and_on_an_unset_key_does_nothing() {
    // A default has no file to take it out of.
    let (fake, _) = tabbed("tui.hover", value("true"), "default", PERSON);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.hover", "true")]);
    assert!(resets(&fake).is_empty());
    let (fake, _) = tabbed("hub.port", Shown::Unset, "default", ANY);
    assert!(fake.writes().is_empty());
    assert!(resets(&fake).is_empty());
}

#[test]
fn a_refused_destination_leaves_the_value_where_it_was() {
    let fake = Fake::new(with("tui.rail.width", value("30"), "global"));
    refuse(&fake, "refused");
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Tab);
    assert_eq!(settings.said, ["refused"]);
    // Nothing was removed from the source.
    assert!(resets(&fake).is_empty());
}

#[test]
fn a_refused_removal_reports_the_partial_move_and_rereads_both_copies() {
    let fake = Fake::new(with("tui.rail.width", value("30"), "global"));
    *support::lock(&fake.refuse_reset) = Some("changed meanwhile".to_owned());
    let mut settings = at(&fake, "tui.rail.width");
    let reads = fake.reads().len();
    one(&mut settings, &fake, Key::Tab);
    assert_eq!(writes(&fake).len(), 1);
    assert_eq!(resets(&fake).len(), 1);
    assert_eq!(fake.reads().len(), reads + 1, "the rows were read again");
    let said = settings.said.join(" ");
    assert!(said.contains("Saved to project config"), "{said}");
    assert!(
        said.contains("removing it from global failed: changed meanwhile"),
        "{said}"
    );
    assert!(!said.contains("Moved"), "{said}");
}

#[test]
fn delete_removes_the_override_from_the_layer_it_comes_from() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(resets(&fake), [w(Layer::Project, "tui.rail.width", "30")]);
    assert_eq!(
        settings.said,
        ["Removed tui.rail.width from project config."]
    );
    // A default value, and an unset key, have nothing to remove.
    let mut hover = at(&fake, "tui.hover");
    edit(&mut hover, &fake, Edit::Delete);
    let mut port = at(&fake, "hub.port");
    edit(&mut port, &fake, Edit::Delete);
    assert_eq!(resets(&fake).len(), 1);
}

#[test]
fn a_refused_reset_says_why_and_leaves_the_row() {
    let fake = Fake::new(rows());
    *support::lock(&fake.refuse_reset) = Some("changed meanwhile".to_owned());
    let mut settings = at(&fake, "tui.rail.width");
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(settings.said, ["changed meanwhile"]);
    assert_eq!(
        row_of(&screen(&settings), "Rail width"),
        "› Rail width ▕█████░░░▏ 30% of screen width · 24 columns project"
    );
}

#[test]
fn a_remote_terminals_client_keys_offer_only_global() {
    // The remote seam narrows a client-owned row to global before the
    // view sees it: Tab has nowhere to go.
    let mut all = with("tui.rail.width", value("30"), "global");
    for row in &mut all {
        row.scope = WriteScope::GlobalOnly;
    }
    let fake = Fake::new(all);
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Tab);
    assert!(fake.writes().is_empty());
    assert!(resets(&fake).is_empty());
    // A change still saves, to global.
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.rail.width", "31")]);
}

#[test]
fn layers_follow_each_write_scope() {
    assert_eq!(
        layers(WriteScope::Any { repo: true }),
        [Layer::Global, Layer::Project, Layer::Repository]
    );
    assert_eq!(
        layers(WriteScope::Any { repo: false }),
        [Layer::Global, Layer::Project]
    );
    assert_eq!(
        layers(WriteScope::PersonFiles),
        [Layer::Global, Layer::Project]
    );
    assert_eq!(layers(WriteScope::GlobalOnly), [Layer::Global]);
    assert_eq!(layers(WriteScope::RepoOnly), [Layer::Repository]);
}

#[test]
fn a_repository_only_key_saves_to_the_repository_never_global() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "repository_extensions");
    // Enter opens the page, Enter on "+ Add" opens its field.
    press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    typed(&mut settings, &fake, "ext");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(
            Layer::Repository,
            "repository_extensions",
            r#"[{"path":"ext","required":false}]"#
        )]
    );
}

#[test]
fn enter_on_an_unbounded_number_opens_its_text_field() {
    let fake = Fake::new(vec![on(
        SettingsTab::Advanced,
        "Unbounded number",
        "test.unbounded_number",
        value("17"),
        "project",
        ANY,
        SettingControl::Number,
    )]);
    let mut settings = at(&fake, "test.unbounded_number");
    one(&mut settings, &fake, Key::Enter);
    assert!(matches!(settings.mode, super::Mode::Field(_)));
    assert!(fake.writes().is_empty());
}

#[test]
fn ctrl_g_opens_the_focused_rows_file_and_the_global_one_for_a_default() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model");
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(
        matches!(&act, Act::Open(path) if *path == file(Layer::Global)),
        "{act:?}"
    );
    let mut settings = at(&fake, "tui.rail.width");
    // A client-owned key opens the client's file.
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(
        matches!(&act, Act::OpenLocal(Some(path)) if *path == file(Layer::Project)),
        "{act:?}"
    );
    let mut settings = at(&fake, "hub.port");
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(
        matches!(&act, Act::Open(path) if *path == fake.global),
        "{act:?}"
    );
    let mut settings = at(&fake, "tui.hover");
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(matches!(act, Act::OpenLocal(None)), "{act:?}");
}

/// ↑↓ move between rows and wrap from the last row to the first and
/// back (`docs/tui.md:1123-1124`, `/settings`).
#[test]
fn up_and_down_wrap_between_the_first_and_last_rows() {
    let fake = Fake::new(rows());
    let mut settings = tab(&fake, SettingsTab::Models);
    one(&mut settings, &fake, Key::Down);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.models_dev".to_owned()))
    );
    // A move clears what was said.
    settings.said = vec!["Saved.".to_owned()];
    one(&mut settings, &fake, Key::Down);
    assert!(settings.said.is_empty());
    // Down the last three rows to the screen row ending the tab.
    for _ in 0..3 {
        one(&mut settings, &fake, Key::Down);
    }
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(Screen::Credentials))
    );
    // ↓ on the last row wraps to the first.
    one(&mut settings, &fake, Key::Down);
    assert_eq!(settings.focus, Some(super::Focus::Key("model".to_owned())));
    // ↑ on the first row wraps to the last.
    one(&mut settings, &fake, Key::Up);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(Screen::Credentials))
    );
    // Pages stay clamped at either end.
    one(&mut settings, &fake, Key::PageDown);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(Screen::Credentials))
    );
    one(&mut settings, &fake, Key::Up);
    one(&mut settings, &fake, Key::PageUp);
    assert_eq!(settings.focus, Some(super::Focus::Key("model".to_owned())));
}

/// The list sub-page wraps ↑↓ past either end while pages stay
/// clamped (`docs/tui.md:666-667`, "Swapped views").
#[test]
fn a_sub_page_wraps_up_and_down_past_either_end() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    assert_eq!(page_at(&settings), 0);
    // ↑ on the first entry wraps to "+ Add" past the last.
    one(&mut settings, &fake, Key::Up);
    assert_eq!(page_at(&settings), 2);
    // ↓ on "+ Add" wraps to the first entry.
    one(&mut settings, &fake, Key::Down);
    assert_eq!(page_at(&settings), 0);
    // Between entries it still steps.
    one(&mut settings, &fake, Key::Down);
    assert_eq!(page_at(&settings), 1);
    // Pages stay clamped at either end.
    one(&mut settings, &fake, Key::PageUp);
    assert_eq!(page_at(&settings), 0);
    one(&mut settings, &fake, Key::PageDown);
    assert_eq!(page_at(&settings), 2);
    one(&mut settings, &fake, Key::PageDown);
    assert_eq!(page_at(&settings), 2);
}

/// The theme choices wrap ↑↓ past either end, and the preview
/// follows the wrapped choice (`docs/tui.md:666-667`, "Swapped views").
#[test]
fn the_theme_choices_wrap_up_and_down_past_either_end() {
    let fake = themed(rows());
    let mut settings = theme_choices(&fake);
    assert_eq!(choice_at(&settings), 0);
    // ↑ on the first choice wraps to the last and previews it.
    let act = one(&mut settings, &fake, Key::Up);
    assert_eq!(choice_at(&settings), 4);
    assert!(matches!(act, Act::Theme(_)), "{act:?}");
    // ↓ on the last choice wraps to the first.
    let act = one(&mut settings, &fake, Key::Down);
    assert_eq!(choice_at(&settings), 0);
    assert!(matches!(act, Act::Theme(_)), "{act:?}");
}

#[test]
fn a_reread_keeps_the_focused_key_where_it_is_and_a_vanished_one_falls_to_the_first() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "scoped_models");
    let mut fewer = rows();
    fewer.retain(|row| row.key != "model");
    *support::lock(&fake.rows) = Ok(fewer);
    settings.reread(&ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("scoped_models".to_owned()))
    );
    let mut none = rows();
    none.retain(|row| row.key != "scoped_models");
    *support::lock(&fake.rows) = Ok(none);
    settings.reread(&ctx(&fake));
    assert_eq!(settings.focus, Some(super::Focus::Key("model".to_owned())));
}

#[test]
fn a_click_focuses_a_row_and_a_blank_line_or_the_header_changes_nothing() {
    let fake = Fake::new(rows());
    let mut settings = tab(&fake, SettingsTab::Models);
    let layout = settings.layout(80, 24, None);
    let second = layout.takes[1];
    settings.click(Spot::Row(second), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.models_dev".to_owned()))
    );
    // The line between two rows is not a row.
    settings.click(Spot::Row(second + 1), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.models_dev".to_owned()))
    );
    settings.click(Spot::Row(500), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.models_dev".to_owned()))
    );
    assert!(matches!(
        settings.click(Spot::Close, &ctx(&fake)),
        Act::Close
    ));
}

#[test]
fn the_renderer_targets_each_drawn_line_so_a_click_lands_on_the_row_it_shows() {
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    render(
        &settings.frame(80, 24, None, 80),
        area,
        &mut buf,
        &mut targets,
    );
    let shown = text_of(&buf, 80, 24);
    for label in ["Default model", "Scoped models", "Credentials"] {
        let y =
            u16::try_from(shown.lines().position(|line| line.contains(label)).unwrap()).unwrap();
        let spot = targets
            .iter()
            .filter(|target| target.rect.y == y)
            .find_map(|target| {
                if let crate::mouse::TargetId::View(Spot::Row(at)) = target.id {
                    Some(at)
                } else {
                    None
                }
            })
            .unwrap();
        let mut clicked = tab(&fake, SettingsTab::Models);
        clicked.click(Spot::Row(spot), &ctx(&fake));
        let focused = clicked
            .items()
            .into_iter()
            .find(|item| clicked.focus_of(*item) == clicked.focus)
            .map(|item| clicked.label_of(item));
        assert_eq!(
            focused.as_deref(),
            Some(match label {
                "Default model" => "Default model",
                other => other,
            })
        );
    }
}

#[test]
fn the_wheel_scrolls_three_rows_and_settles_the_focus_on_a_row() {
    let fake = Fake::new(rows());
    let small = sized(&fake, 80, 10);
    let mut settings = Settings::open(&small, Vec::new());
    for _ in 0..3 {
        settings.edit_key(Edit::Right, &small);
    }
    // Permissions: 3 keys and 2 screen rows packed into the short body.
    let before = settings.list.top();
    settings.wheel(false, &small);
    let layout = settings.layout(80, 10, None);
    assert!(settings.list.top() >= before);
    assert!(layout.takes.iter().any(|line| Some(*line) == layout.focus));
    // The wheel never writes.
    assert!(fake.writes().is_empty());
}

// The list sub-page.

/// The view over `fake` with the list of `key` open.
fn page(fake: &Fake, key: &str) -> Settings {
    let mut settings = at(fake, key);
    one(&mut settings, fake, Key::Enter);
    assert!(settings.page.is_some(), "{key} opens no page");
    settings
}

fn entries(settings: &Settings) -> Vec<serde_json::Value> {
    settings
        .page
        .as_ref()
        .map(|page| page.entries.clone())
        .unwrap_or_default()
}

/// The sub-page's focused row.
fn page_at(settings: &Settings) -> usize {
    settings.page.as_ref().unwrap().list.selected()
}

fn extensions(shown: &str) -> Vec<SettingRow> {
    with("repository_extensions", value(shown), "repository")
}

#[test]
fn a_list_opens_its_own_page_titled_with_its_label() {
    let fake = Fake::new(rows());
    let settings = page(&fake, "scoped_models");
    expect(
        &screen(&settings),
        r#"
| Scoped models  /settings                                            Esc returns
|› a/b
|  c/d
|  + Add
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|Enter edit · Delete remove · ⌥↑⌥↓ move · Esc back
"#,
    );
    assert_eq!(settings.frame(80, 24, None, 80).title, "Scoped models");
}

#[test]
fn an_empty_list_shows_only_add_and_an_object_list_shows_path_and_required() {
    let fake = Fake::new(rows());
    let empty = page(&fake, "sandbox.allow_write");
    expect(
        &screen(&empty),
        r#"
| Sandbox writable paths  /settings                                   Esc returns
|› + Add
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|Enter edit · Delete remove · ⌥↑⌥↓ move · Esc back
"#,
    );
    let fake = Fake::new(extensions(
        r#"[{"path":"ext/a","required":true},{"path":"ext/bb"}]"#,
    ));
    let objects = page(&fake, "repository_extensions");
    expect(
        &screen(&objects),
        r#"
| Repository extensions  /settings                                    Esc returns
|› ext/a   [x] required
|  ext/bb  [ ] required
|  + Add
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|Enter edit · Delete remove · ⌥↑⌥↓ move · Esc back
"#,
    );
}

#[test]
fn the_pages_footer_has_the_documented_words_with_bold_keys() {
    let fake = Fake::new(rows());
    let settings = page(&fake, "scoped_models");
    let frame = settings.frame(80, 24, None, 80);
    assert_eq!(
        frame.footer,
        "Enter edit · Delete remove · ⌥↑⌥↓ move · Esc back"
    );
    let spans = frame.chrome.unwrap().footer;
    let bold: Vec<&str> = spans
        .iter()
        .filter(|(_, style)| style.add_modifier.contains(Modifier::BOLD))
        .map(|(text, _)| text.as_str())
        .collect();
    assert_eq!(bold, ["Enter", "Delete", "⌥↑⌥↓", "Esc"]);
    let text: String = spans.iter().map(|(text, _)| text.as_str()).collect();
    assert_eq!(text, frame.footer);
}

#[test]
fn adding_saves_the_whole_list_once_at_the_layer_the_value_comes_from() {
    let fake = Fake::new(with("scoped_models", value(r#"["a/b","c/d"]"#), "project"));
    let mut settings = page(&fake, "scoped_models");
    press(&mut settings, &fake, &[Key::Down, Key::Down, Key::Enter]);
    expect(
        &screen(&settings),
        r#"
| Scoped models  /settings                                            Esc returns
|  a/b
|  c/d
|›
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|Enter save · Esc cancel
"#,
    );
    typed(&mut settings, &fake, "e/f");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "scoped_models", r#"["a/b","c/d","e/f"]"#)]
    );
    assert_eq!(entries(&settings).len(), 3);
    assert!(settings.page.as_ref().unwrap().field.is_none());
    // The new entry is focused.
    assert_eq!(settings.page.as_ref().unwrap().list.selected(), 2);
}

#[test]
fn editing_opens_the_entrys_text_and_saves_the_whole_list() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    one(&mut settings, &fake, Key::Down);
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        settings
            .page
            .as_ref()
            .unwrap()
            .field
            .as_ref()
            .unwrap()
            .draft
            .expand(),
        "c/d"
    );
    press(&mut settings, &fake, &[Key::Backspace]);
    typed(&mut settings, &fake, "x");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "scoped_models", r#"["a/b","c/x"]"#)]
    );
}

#[test]
fn delete_removes_the_focused_entry_and_the_last_one_leaves_an_empty_list() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "scoped_models", r#"["c/d"]"#)]
    );
    assert!(
        resets(&fake).is_empty(),
        "an entry's Delete is not the parent's reset"
    );
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "scoped_models", "[]"))
    );
    assert!(entries(&settings).is_empty());
    // Delete on "+ Add" removes nothing.
    let count = writes(&fake).len();
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(writes(&fake).len(), count);
}

#[test]
fn option_up_and_down_move_the_focused_entry_one_place_and_stop_at_the_ends() {
    let fake = Fake::new(with("scoped_models", value(r#"["a","b","c"]"#), "global"));
    let mut settings = page(&fake, "scoped_models");
    // Up at the top, and either key on "+ Add", move nothing.
    one(&mut settings, &fake, Key::AltUp);
    assert!(fake.writes().is_empty());
    one(&mut settings, &fake, Key::AltDown);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "scoped_models", r#"["b","a","c"]"#)]
    );
    assert_eq!(
        settings.page.as_ref().unwrap().list.selected(),
        1,
        "focus follows the entry"
    );
    one(&mut settings, &fake, Key::AltDown);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "scoped_models", r#"["b","c","a"]"#))
    );
    // Down at the last entry moves nothing.
    let count = writes(&fake).len();
    one(&mut settings, &fake, Key::AltDown);
    assert_eq!(writes(&fake).len(), count);
    one(&mut settings, &fake, Key::AltUp);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "scoped_models", r#"["b","a","c"]"#))
    );
    assert_eq!(settings.page.as_ref().unwrap().list.selected(), 1);
    // On "+ Add" neither moves.
    press(&mut settings, &fake, &[Key::Down, Key::Down]);
    let count = writes(&fake).len();
    press(&mut settings, &fake, &[Key::AltUp, Key::AltDown]);
    assert_eq!(writes(&fake).len(), count);
}

#[test]
fn a_refused_save_keeps_the_entries_the_focus_and_an_open_field() {
    let fake = Fake::new(with("scoped_models", value(r#"["a","b"]"#), "global"));
    let mut settings = page(&fake, "scoped_models");
    refuse(&fake, "scoped_models must be strings");
    // Remove, reorder and flip fail without losing the entry or its place.
    one(&mut settings, &fake, Key::Down);
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(entries(&settings).len(), 2);
    one(&mut settings, &fake, Key::AltUp);
    assert_eq!(
        entries(&settings),
        [serde_json::json!("a"), serde_json::json!("b")]
    );
    assert_eq!(settings.page.as_ref().unwrap().list.selected(), 1);
    // An add keeps its field and draft, with the error under it.
    press(&mut settings, &fake, &[Key::Down, Key::Enter]);
    typed(&mut settings, &fake, "zz");
    one(&mut settings, &fake, Key::Enter);
    let field = settings.page.as_ref().unwrap().field.as_ref().unwrap();
    assert_eq!(field.draft.expand(), "zz");
    assert_eq!(settings.said, ["scoped_models must be strings"]);
    expect(
        &screen(&settings),
        r#"
| Scoped models  /settings                                            Esc returns
|  a
|  b
|› zz
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|scoped_models must be strings
|Enter save · Esc cancel
"#,
    );
    // Retry once the save works.
    *support::lock(&fake.refuse) = None;
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake).last(),
        Some(&w(Layer::Global, "scoped_models", r#"["a","b","zz"]"#))
    );
}

#[test]
fn esc_closes_the_field_first_then_the_page_with_the_list_row_focused() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    press(&mut settings, &fake, &[Key::Enter, Key::Char('x')]);
    one(&mut settings, &fake, Key::Esc);
    let open = settings.page.as_ref().unwrap();
    assert!(open.field.is_none());
    assert!(fake.writes().is_empty());
    // The second Esc returns to the tab, still on the list's row.
    let act = one(&mut settings, &fake, Key::Esc);
    assert!(matches!(act, Act::Stay));
    assert!(settings.page.is_none());
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("scoped_models".to_owned()))
    );
    assert!(screen(&settings).contains("› Scoped models"));
}

#[test]
fn an_empty_entry_is_the_schemas_to_judge() {
    // `fiber config set` takes an empty string member, so the page sends it
    // as typed instead of refusing it first.
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "sandbox.allow_write");
    press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "sandbox.allow_write", r#"[""]"#)]
    );
    assert!(settings.page.as_ref().unwrap().field.is_none());
    // A refusal from the schema keeps the field and says why.
    let refused = Fake::new(rows());
    let mut settings = page(&refused, "sandbox.allow_write");
    *support::lock(&refused.refuse) = Some("sandbox.allow_write is refused".to_owned());
    press(&mut settings, &refused, &[Key::Enter, Key::Enter]);
    assert!(settings.page.as_ref().unwrap().field.is_some());
    assert_eq!(settings.said, ["sandbox.allow_write is refused"]);
}

#[test]
fn space_flips_required_and_a_new_entry_starts_with_it_off() {
    let fake = Fake::new(extensions(
        r#"[{"path":"ext/a","required":true},{"path":"ext/b","note":"keep"}]"#,
    ));
    let mut settings = page(&fake, "repository_extensions");
    one(&mut settings, &fake, Key::Char(' '));
    let first = writes(&fake);
    assert_eq!(first.len(), 1);
    assert_eq!(
        (first[0].0, first[0].1.as_str()),
        (Layer::Repository, "repository_extensions")
    );
    let parsed: serde_json::Value = serde_json::from_str(&first[0].2).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!([{"path":"ext/a","required":false},{"path":"ext/b","note":"keep"}])
    );
    // An absent `required` reads off; flipping it turns it on, keeping
    // every other member.
    one(&mut settings, &fake, Key::Down);
    one(&mut settings, &fake, Key::Char(' '));
    let last = writes(&fake).last().cloned().unwrap().2;
    let parsed: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert_eq!(
        parsed[1],
        serde_json::json!({"path":"ext/b","note":"keep","required":true})
    );
    // A new entry has `required` off.
    press(&mut settings, &fake, &[Key::Down, Key::Enter]);
    typed(&mut settings, &fake, "ext/c");
    one(&mut settings, &fake, Key::Enter);
    let last: serde_json::Value =
        serde_json::from_str(&writes(&fake).last().cloned().unwrap().2).unwrap();
    assert_eq!(
        last[2],
        serde_json::json!({"path":"ext/c","required":false})
    );
}

#[test]
fn editing_an_object_changes_its_path_and_keeps_its_other_members() {
    let fake = Fake::new(extensions(
        r#"[{"path":"ext/a","required":true,"note":"keep"}]"#,
    ));
    let mut settings = page(&fake, "repository_extensions");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        settings
            .page
            .as_ref()
            .unwrap()
            .field
            .as_ref()
            .unwrap()
            .draft
            .expand(),
        "ext/a"
    );
    typed(&mut settings, &fake, "2");
    one(&mut settings, &fake, Key::Enter);
    let saved: serde_json::Value = serde_json::from_str(&writes(&fake)[0].2).unwrap();
    assert_eq!(
        saved,
        serde_json::json!([{"path":"ext/a2","required":true,"note":"keep"}])
    );
    // Space on a string list does nothing.
    let fake = Fake::new(rows());
    let mut strings = page(&fake, "scoped_models");
    one(&mut strings, &fake, Key::Char(' '));
    assert!(fake.writes().is_empty());
}

#[test]
fn each_kind_of_change_saves_the_whole_list_once() {
    for (label, keys, edit_, want) in [
        (
            "add",
            vec![Key::Down, Key::Down, Key::Enter, Key::Char('z'), Key::Enter],
            None,
            r#"["a","b","z"]"#,
        ),
        (
            "edit",
            vec![Key::Enter, Key::Char('z'), Key::Enter],
            None,
            r#"["az","b"]"#,
        ),
        ("remove", vec![], Some(Edit::Delete), r#"["b"]"#),
        ("move", vec![Key::AltDown], None, r#"["b","a"]"#),
    ] {
        let fake = Fake::new(with("scoped_models", value(r#"["a","b"]"#), "project"));
        let mut settings = page(&fake, "scoped_models");
        press(&mut settings, &fake, &keys);
        if let Some(edit_) = edit_ {
            edit(&mut settings, &fake, edit_);
        }
        assert_eq!(
            writes(&fake),
            [w(Layer::Project, "scoped_models", want)],
            "{label}"
        );
    }
    let fake = Fake::new(extensions(r#"[{"path":"a","required":false}]"#));
    let mut settings = page(&fake, "repository_extensions");
    one(&mut settings, &fake, Key::Char(' '));
    assert_eq!(writes(&fake).len(), 1, "flip");
}

#[test]
fn deleting_the_list_row_resets_it_and_deleting_in_the_page_removes_an_entry() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "scoped_models");
    edit(&mut settings, &fake, Edit::Delete);
    assert_eq!(
        resets(&fake),
        [w(Layer::Global, "scoped_models", r#"["a/b","c/d"]"#)]
    );
    assert!(fake.writes().is_empty());
}

#[test]
fn the_pages_clicks_and_wheel_move_its_focus_only_outside_a_field() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    settings.click(Spot::Row(1), &ctx(&fake));
    assert_eq!(settings.page.as_ref().unwrap().list.selected(), 1);
    one(&mut settings, &fake, Key::Enter);
    settings.click(Spot::Row(0), &ctx(&fake));
    assert_eq!(
        settings.page.as_ref().unwrap().list.selected(),
        1,
        "a field holds the focus"
    );
    assert!(fake.writes().is_empty());
    settings.wheel(true, &ctx(&fake));
    assert!(fake.writes().is_empty());
}

// The theme choices `/theme` opens on.

/// The view `/theme` opens: the theme choices over the theme row.
fn theme_choices(fake: &Fake) -> Settings {
    Settings::open_theme(&ctx(fake), Vec::new())
}

/// The theme choices' names while they are open.
fn names(settings: &Settings) -> Vec<String> {
    match &settings.mode {
        super::Mode::Choices { names, .. } => names.clone(),
        super::Mode::Rows | super::Mode::Field(_) => Vec::new(),
    }
}

/// The theme choices' focused row.
fn choice_at(settings: &Settings) -> usize {
    let super::Mode::Choices { list, .. } = &settings.mode else {
        panic!("no choices");
    };
    list.selected()
}

#[test]
fn the_theme_command_opens_on_auto_dark_light_then_the_theme_files() {
    let fake = themed(rows());
    let settings = theme_choices(&fake);
    assert_eq!(names(&settings), ["auto", "dark", "light", "dusk", "solar"]);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.theme".to_owned()))
    );
    expect(
        &screen(&settings),
        r#"
| Settings › tui.theme  /settings                                     Esc returns
|› auto
|  dark
|  light
|  dusk
|  solar
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|↑↓ move · Enter choose · Esc back
"#,
    );
}

#[test]
fn the_current_theme_is_marked_and_selected() {
    let fake = themed(with("tui.theme", value("\"solar\""), "global"));
    let settings = theme_choices(&fake);
    let shown = screen(&settings);
    assert!(shown.contains("solar  (current)"), "{shown}");
    assert!(!shown.contains("dusk  (current)"), "{shown}");
    let super::Mode::Choices { list, .. } = &settings.mode else {
        panic!("no choices");
    };
    assert_eq!(list.selected(), 4);
}

#[test]
fn choosing_a_theme_writes_the_global_tui_theme_queues_it_and_returns_to_the_tabs() {
    let fake = themed(rows());
    let mut settings = theme_choices(&fake);
    let acts = press(&mut settings, &fake, &[Key::Down, Key::Down, Key::Enter]);
    assert!(
        matches!(acts.first(), Some(Act::Theme(ThemeSetting::Dark))),
        "{acts:?}"
    );
    assert!(
        matches!(acts.last(), Some(Act::Theme(ThemeSetting::Light))),
        "{acts:?}"
    );
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.theme", "\"light\"")]);
    assert!(names(&settings).is_empty());
    assert_eq!(settings.said, ["Saved to global config."]);
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|  Hover         [x] On
|
|  Rail width    ▕█████░░░▏ 30% of screen width · 24 columns  project
|
|› Theme         Default
|
|  Key bindings  Open /keys ›
|
|
|
|
|
|
|
|
|About tui.theme.
|tui.theme · not set · Applies now.
|Saved to global config.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn a_click_on_a_choice_chooses_it() {
    let fake = themed(rows());
    let mut settings = theme_choices(&fake);
    let act = settings.click(Spot::Row(4), &ctx(&fake));
    assert!(
        matches!(&act, Act::Theme(ThemeSetting::File { name, .. }) if name == "solar"),
        "{act:?}"
    );
}

#[test]
fn a_refused_choice_queues_nothing_and_keeps_the_choices() {
    let fake = themed(rows());
    refuse(&fake, "refused");
    let mut settings = theme_choices(&fake);
    let act = one(&mut settings, &fake, Key::Enter);
    assert!(matches!(act, Act::Stay), "{act:?}");
    assert_eq!(settings.said, ["refused"]);
    assert!(!names(&settings).is_empty());
}

#[test]
fn esc_restores_the_entry_theme_and_writes_nothing() {
    let fake = themed(with("tui.theme", value("\"dark\""), "global"));
    let mut settings = theme_choices(&fake);
    one(&mut settings, &fake, Key::Down);
    let act = one(&mut settings, &fake, Key::Esc);
    assert!(matches!(act, Act::Theme(ThemeSetting::Dark)), "{act:?}");
    assert!(fake.writes().is_empty());
    assert!(settings.choices_entry().is_none());
}

#[test]
fn moving_in_the_choices_previews_the_focused_theme() {
    let fake = themed(rows());
    let mut settings = theme_choices(&fake);
    let act = one(&mut settings, &fake, Key::Down);
    assert!(matches!(act, Act::Theme(ThemeSetting::Dark)), "{act:?}");
    assert!(fake.writes().is_empty());
}

#[test]
fn applies_by_key() {
    for (key, when) in [
        ("tui.theme", Applies::Now),
        ("tui.hover", Applies::TerminalStart),
        ("hub.port", Applies::HubStart),
        ("hubs.x.address", Applies::NextConnection),
        ("hub.default", Applies::NextConnection),
        ("hub.idle_exit_ms", Applies::HubStart),
        ("session.keep_exited", Applies::HubStart),
        ("diagnostics.level", Applies::ProcessStart),
        ("model", Applies::NewSessions),
        ("thinking", Applies::NewSessions),
        ("models.x.thinking", Applies::NewSessions),
        ("models.openai/gpt.6.thinking", Applies::NewSessions),
        ("providers.openai.credential", Applies::NewSessions),
        ("providers.openai.credentials", Applies::NewSessions),
        ("providers.openai.credentials.work", Applies::NewSessions),
        ("providers.openai.reviewer_model", Applies::Reload),
        ("models.x.price", Applies::Reload),
        ("models.x.thinking.extra", Applies::Reload),
        ("providers.openai.credential.extra", Applies::Reload),
        ("providers.openai.credentials_extra", Applies::Reload),
        ("skills.disabled", Applies::NextTurn),
        ("compaction.tokens", Applies::Reload),
    ] {
        assert_eq!(applies(key), when, "{key}");
    }
}

#[test]
fn a_credential_write_applies_to_new_sessions() {
    assert_eq!(
        applies_text("providers.openai.credential", None),
        "Applies to new sessions."
    );
}

#[test]
fn client_hub_keys_apply_at_the_next_connection() {
    for key in ["hub.default", "hubs.box.address"] {
        assert_eq!(
            applies_text(key, None),
            "Applies at this client's next connection."
        );
    }
}

#[test]
fn the_reload_cost_uses_the_last_usage() {
    assert_eq!(
        applies_text("compaction.tokens", Some(180_000)),
        "changing compaction.tokens needs a reload (~180k tokens)"
    );
    assert_eq!(
        applies_text("compaction.tokens", Some(999)),
        "changing compaction.tokens needs a reload (~999 tokens)"
    );
}

#[test]
fn without_usage_it_says_a_change_needs_a_reload() {
    assert_eq!(
        applies_text("compaction.tokens", None),
        "changing compaction.tokens needs a reload"
    );
    assert_eq!(
        applies_text("budget.usd", None),
        "changing budget.usd needs a reload"
    );
}

#[test]
fn as_typed_strips_a_strings_quotes_only() {
    assert_eq!(as_typed("\"a/b\""), "a/b");
    assert_eq!(as_typed("400000"), "400000");
    assert_eq!(as_typed("[\"a\"]"), "[\"a\"]");
    assert_eq!(as_typed("not json"), "not json");
}

/// One screen row as the contract spells it: label, tab, command, covered
/// schema entries.
type Contract = (
    &'static str,
    SettingsTab,
    &'static str,
    &'static [&'static str],
);

const SCREENS: [Contract; 6] = [
    (
        "Key bindings",
        SettingsTab::Appearance,
        "/keys",
        &["keys.*"],
    ),
    (
        "Credentials",
        SettingsTab::Models,
        "/credential",
        &["providers.*.credential"],
    ),
    ("Rules", SettingsTab::Permissions, "/rules", &[]),
    (
        "Tools",
        SettingsTab::Permissions,
        "/tools",
        &[
            "tools.*.max_result_bytes",
            "tools.*.deferred",
            "extensions.*.tools.enabled",
            "extensions.*.tools.disabled",
        ],
    ),
    (
        "Skills",
        SettingsTab::Sessions,
        "/skills",
        &["skills.disabled"],
    ),
    (
        "MCP servers",
        SettingsTab::Sessions,
        "/mcp",
        &[
            "mcp.servers.*.command",
            "mcp.servers.*.args",
            "mcp.servers.*.env",
            "mcp.servers.*.url",
            "mcp.servers.*.required",
            "mcp.servers.*.startup_timeout_ms",
            "mcp.servers.*.timeout_ms",
            "mcp.servers.*.declare_in_full",
            "mcp.servers.*.tools.enabled",
            "mcp.servers.*.tools.disabled",
            "mcp.servers.*.tools.*.hints",
        ],
    ),
];

#[test]
fn settings_metadata_screen_rows_are_complete() {
    for row in SCREEN_ROWS {
        let ScreenRow { metadata, .. } = row;
        assert!(metadata.tab.is_some(), "`{}` has no tab", metadata.label);
        for (name, text) in [
            ("label", metadata.label),
            ("description", metadata.description),
        ] {
            assert!(!text.trim().is_empty(), "`{}` has no {name}", row.command);
            assert_eq!(text.trim(), text, "`{}` {name} has spaces", row.command);
            assert!(
                !text.contains('\n'),
                "`{}` {name} is more than one line",
                row.command
            );
        }
    }
}

#[test]
fn settings_metadata_screen_rows_match_the_contract() {
    // The contract orders the rows, not the entries a row covers.
    let covered_set = |covers: &[&'static str]| {
        let mut set = covers.to_vec();
        set.sort_unstable();
        set.dedup();
        set
    };
    let rows: Vec<_> = SCREEN_ROWS
        .iter()
        .map(|row| {
            let tab = row.metadata.tab.expect("a screen row has a tab");
            (
                row.metadata.label,
                tab,
                row.command,
                covered_set(row.covers),
            )
        })
        .collect();
    let want: Vec<_> = SCREENS
        .iter()
        .map(|&(label, tab, command, covers)| (label, tab, command, covered_set(covers)))
        .collect();
    assert_eq!(rows, want, "label, tab, command and coverage, in order");

    // The set comparison above hides a repeated entry.
    let mut covered: Vec<&str> = SCREEN_ROWS
        .iter()
        .flat_map(|row| row.covers)
        .copied()
        .collect();
    let all = covered.len();
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(covered.len(), all, "a path is covered twice");
}

#[test]
fn settings_metadata_screen_commands_are_registered() {
    for row in SCREEN_ROWS {
        let name = row
            .command
            .strip_prefix('/')
            .unwrap_or_else(|| panic!("`{}` is not a slash command", row.command));
        assert!(
            contract::commands::BUILT_IN_COMMANDS.contains(&name),
            "`{}` is not a built-in command",
            row.command
        );
    }
}

// Stored whole-screen snapshots for ordinary, slider and stepped-list rows.

#[test]
fn settings_80x24() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "compaction.tokens");
    insta::assert_snapshot!("settings_80x24", drawn(&settings, Some(180_000), 80, 24));
}

#[test]
fn settings_editing() {
    let mut all = rows();
    all.push(on(
        SettingsTab::Appearance,
        "Editor",
        "tui.editor",
        value("\"vim\""),
        "project",
        PERSON,
        SettingControl::Text,
    ));
    let fake = Fake::new(all);
    let mut settings = at(&fake, "tui.editor");
    one(&mut settings, &fake, Key::Enter);
    typed(&mut settings, &fake, "5");
    let super::Mode::Field(field) = &settings.mode else {
        panic!("no field");
    };
    assert_eq!(field.draft.expand(), "vim5");
    insta::assert_snapshot!("settings_editing", drawn(&settings, None, 80, 24));
    assert!(fake.writes().is_empty());
}

#[test]
fn settings_slider_row() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "tui.rail.width");
    insta::assert_snapshot!("settings_slider_row", drawn(&settings, None, 80, 24));
}

#[test]
fn settings_list_row() {
    let fake = Fake::new(rows());
    let settings = at(&fake, "compaction.tokens");
    insta::assert_snapshot!("settings_list_row", drawn(&settings, None, 80, 24));
}

// Geometry, scrolling and redaction.

/// The layout of the open tab with `lines` things said, `height` tall.
fn said_layout(lines: usize, height: usize) -> super::Layout {
    let fake = Fake::new(rows());
    let mut settings = open(&fake);
    settings.said = (0..lines).map(|at| format!("said {at}")).collect();
    settings.layout(80, height, None)
}

#[test]
fn a_frame_is_compact_below_eighteen_rows_and_spaced_from_eighteen() {
    // Compact: the note and the tab bar. Tall: a blank under each.
    assert_eq!(said_layout(0, 17).top.len(), 2);
    assert_eq!(said_layout(0, 18).top.len(), 4);
}

#[test]
fn the_lines_under_the_list_leave_the_controls_their_rows() {
    // Tall: 18 rows less the title and footer, four fixed rows and three
    // controls' rows leave nine lines under the list, the latest said.
    let layout = said_layout(12, 18);
    assert_eq!(layout.below.len(), 9);
    assert_eq!(layout.below.last().map(String::as_str), Some("said 11"));
    assert_eq!(layout.body, 3);
    // Compact: two fixed rows leave five.
    let layout = said_layout(12, 12);
    assert_eq!(layout.below.len(), 5);
    assert_eq!(layout.body, 3);
}

/// Every number row shows its unit in its row text, word for word as
/// the `/settings` tables spell it; a file value outside a list reads
/// with "(custom)" after it (`docs/tui.md`, `/settings`, "Controls").
#[test]
fn a_number_row_shows_its_unit_in_row_text() {
    use SettingControl::{Number, Text};
    let text = |key: &str, control: SettingControl, json: &str| {
        let row = on(
            SettingsTab::Advanced,
            "label",
            key,
            value(json),
            "default",
            ANY,
            control,
        );
        shown_value(&row).0
    };
    let number = |key: &str, json: &str| text(key, Number, json);
    // Sliders: any value shows plain with its unit.
    assert_eq!(number("tui.rail.width", "15"), "15% of screen width");
    assert_eq!(number("tui.rail.width", "40"), "40% of screen width");
    assert_eq!(number("tui.panel.width", "21"), "21% of screen width");
    assert_eq!(number("quota.notice_at", "80"), "80% used");
    // The stored value is a fraction: the built-in 0.7 reads as 70%.
    assert_eq!(
        number("compaction.window_fraction", "0.7"),
        "70% of the context window"
    );
    assert_eq!(
        number("compaction.window_fraction", "0.5"),
        "50% of the context window"
    );
    assert_eq!(
        number("compaction.window_fraction", "0.525"),
        "52.5% of the context window (custom)"
    );
    assert_eq!(
        number("compaction.window_fraction", "0.95"),
        "95% of the context window (custom)"
    );
    assert_eq!(
        number("compaction.window_fraction", "0.45"),
        "45% of the context window (custom)"
    );
    assert_eq!(number("cache.warm_cap", "2"), "2 cache lifetimes");
    assert_eq!(number("cache.warm_cap", "1"), "1 cache lifetime");
    assert_eq!(number("cache.warm_cap", "0"), "0 cache lifetimes");
    assert_eq!(number("retry.attempts", "3"), "3 retries");
    assert_eq!(number("retry.attempts", "1"), "1 retry");
    // Token counts.
    assert_eq!(number("compaction.tokens", "400000"), "400k tokens");
    assert_eq!(number("compaction.tokens", "1000000"), "1M tokens");
    assert_eq!(
        number("compaction.tokens", "300000"),
        "300k tokens (custom)"
    );
    assert_eq!(number("compaction.tokens", "1000"), "1k tokens (custom)");
    assert_eq!(number("compaction.tokens", "999"), "999 tokens (custom)");
    assert_eq!(number("compaction.tokens", "0"), "0 tokens (custom)");
    // A stored zero is custom; the unset row is Default.
    assert_eq!(number("budget.usd", "10"), "$10");
    assert_eq!(number("budget.usd", "0"), "$0 (custom)");
    assert_eq!(number("budget.usd", "7"), "$7 (custom)");
    assert_eq!(number("budget.usd", "10.5"), "$10.5 (custom)");
    // Block counts, singular against plural.
    assert_eq!(number("reviewer.block_limits.consecutive", "2"), "2 blocks");
    assert_eq!(number("reviewer.block_limits.consecutive", "1"), "1 block");
    assert_eq!(
        number("reviewer.block_limits.consecutive", "4"),
        "4 blocks (custom)"
    );
    assert_eq!(number("reviewer.block_limits.session", "20"), "20 blocks");
    // Millisecond delays: half seconds, whole seconds, minutes, hours.
    assert_eq!(number("retry.initial_delay_ms", "500"), "0.5 s");
    assert_eq!(number("retry.initial_delay_ms", "1000"), "1 s");
    assert_eq!(number("retry.initial_delay_ms", "999"), "0.999 s (custom)");
    assert_eq!(number("retry.initial_delay_ms", "1500"), "1.5 s (custom)");
    assert_eq!(number("retry.max_delay_ms", "10000"), "10 s");
    assert_eq!(number("retry.max_delay_ms", "60000"), "1 min");
    assert_eq!(number("retry.max_delay_ms", "300000"), "5 min");
    assert_eq!(number("hub.idle_exit_ms", "300000"), "5 min");
    assert_eq!(number("hub.idle_exit_ms", "1800000"), "30 min");
    assert_eq!(number("session.idle_exit_ms", "3600000"), "1 h");
    assert_eq!(number("session.idle_exit_ms", "7200000"), "2 h");
    assert_eq!(number("hub.idle_exit_ms", "3540000"), "59 min (custom)");
    assert_eq!(number("hub.idle_exit_ms", "86400000"), "24 h (custom)");
    assert_eq!(number("retry.max_delay_ms", "0"), "0 min (custom)");
    // A negative delay stays in seconds: the whole-unit words are for
    // values the stepping lists hold.
    assert_eq!(number("retry.max_delay_ms", "-60000"), "-60 s (custom)");
    // Durations keep their unit word; off-list values read custom.
    assert_eq!(text("model_lists.refresh_after", Text, "\"24h\""), "24 h");
    assert_eq!(
        text("model_lists.refresh_after", Text, "\"1440m\""),
        "1440 min"
    );
    assert_eq!(text("model_lists.refresh_after", Text, "\"3d\""), "3 d");
    assert_eq!(
        text("model_lists.refresh_after", Text, "\"90m\""),
        "90 min (custom)"
    );
    assert_eq!(
        text("model_lists.refresh_after", Text, "\"90s\""),
        "90 s (custom)"
    );
    // A lone unit with no count keeps its raw text.
    assert_eq!(text("model_lists.refresh_after", Text, "\"h\""), "h");
    assert_eq!(text("model_lists.refresh_after", Text, "\"soon\""), "soon");
    // Only the refresh row reads duration words: any other Text row keeps
    // its raw text even when it ends in a duration unit.
    assert_eq!(text("tui.editor", Text, "\"24h\""), "24h");
    // A non-ASCII value never splits a character boundary: it keeps its
    // raw text instead of panicking.
    assert_eq!(text("model_lists.refresh_after", Text, "\"é\""), "é");
    // Anything else keeps its raw text with no unit.
    assert_eq!(number("hub.port", "8080"), "8080");
    assert_eq!(number("tui.rail.width", "\"wide\""), "wide");
}

/// A width row ends with its columns on the terminal: the share's
/// columns after the region's limits, read from the terminal width
/// while the draw width keeps the room and the cut (`docs/tui.md`,
/// `/settings`, "Controls").
#[test]
fn slider_bar_clamps_its_endpoints_and_rejects_values_outside_the_range() {
    let shown = |text: &str| value(text);
    let range = (10.0, 40.0, 1.0);
    assert_eq!(
        slider_bar(range, &shown("10")),
        Some("▕░░░░░░░░▏".to_owned())
    );
    assert_eq!(
        slider_bar(range, &shown("40")),
        Some("▕████████▏".to_owned())
    );
    assert_eq!(
        slider_bar(range, &shown("20")),
        Some("▕███░░░░░▏".to_owned())
    );
    assert_eq!(
        slider_bar(range, &shown("9.999")),
        Some("▕░░░░░░░░▏".to_owned())
    );
    assert_eq!(
        slider_bar(range, &shown("40.001")),
        Some("▕████████▏".to_owned())
    );
}

#[test]
fn numeric_controls_match_the_documented_values_and_include_defaults() {
    let sliders = [
        ("tui.rail.width", (10.0, 40.0, 1.0), 15.0),
        ("tui.panel.width", (15.0, 45.0, 1.0), 21.0),
        ("quota.notice_at", (50.0, 95.0, 5.0), 80.0),
        ("compaction.window_fraction", (0.5, 0.9, 0.05), 0.7),
        ("cache.warm_cap", (0.0, 11.0, 1.0), 2.0),
        ("retry.attempts", (0.0, 10.0, 1.0), 3.0),
    ];
    for (key, want, default) in sliders {
        assert_eq!(number_slider(key), Some(want), "{key}");
        assert!((want.0..=want.1).contains(&default), "{key}");
    }
    let lists: [(&str, &[f64], f64); 8] = [
        (
            "compaction.tokens",
            &[
                100_000.0,
                200_000.0,
                400_000.0,
                600_000.0,
                800_000.0,
                1_000_000.0,
            ],
            400_000.0,
        ),
        ("budget.usd", &[0.0, 1.0, 5.0, 10.0, 25.0, 50.0, 100.0], 0.0),
        (
            "reviewer.block_limits.consecutive",
            &[1.0, 2.0, 3.0, 5.0, 10.0],
            3.0,
        ),
        (
            "reviewer.block_limits.session",
            &[5.0, 10.0, 20.0, 50.0, 100.0],
            20.0,
        ),
        (
            "retry.initial_delay_ms",
            &[500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0],
            2_000.0,
        ),
        (
            "retry.max_delay_ms",
            &[10_000.0, 30_000.0, 60_000.0, 120_000.0, 300_000.0],
            60_000.0,
        ),
        (
            "session.idle_exit_ms",
            &[300_000.0, 900_000.0, 1_800_000.0, 3_600_000.0, 7_200_000.0],
            1_800_000.0,
        ),
        ("session.keep_exited", &[0.0, 5.0, 10.0, 20.0, 50.0], 10.0),
    ];
    for (key, want, default) in lists {
        assert_eq!(number_list(key), Some(want), "{key}");
        assert!(want.contains(&default), "{key}");
    }
    assert_eq!(number_list("hub.idle_exit_ms"), None);
    assert_eq!(
        number_list("session.keep_exited"),
        Some(&[0.0, 5.0, 10.0, 20.0, 50.0][..])
    );
    assert_eq!(number_list("tui.rail.width"), None);
    assert_eq!(
        text_list("model_lists.refresh_after"),
        Some(&["1h", "6h", "12h", "24h", "3d", "7d"][..])
    );
    let refresh_values = text_list("model_lists.refresh_after").unwrap();
    assert!(refresh_values.contains(&"24h"));
    assert_eq!(refresh_values.len(), 6);
    // The display and the control share each list table's membership.
    assert_eq!(
        shown_value(&on(
            SettingsTab::Sessions,
            "tokens",
            "compaction.tokens",
            value("400000"),
            "default",
            ANY,
            SettingControl::Number
        ))
        .0,
        "400k tokens"
    );
    assert_eq!(
        shown_value(&on(
            SettingsTab::Sessions,
            "budget",
            "budget.usd",
            value("0"),
            "default",
            ANY,
            SettingControl::Number
        ))
        .0,
        "$0 (custom)"
    );
}

#[test]
fn keep_exited_lists_and_persists_the_documented_session_counts() {
    let row = |shown| {
        on(
            SettingsTab::Sessions,
            "Keep exited",
            "session.keep_exited",
            shown,
            "project",
            ANY,
            SettingControl::Number,
        )
    };
    assert_eq!(super::number_text("session.keep_exited", "0"), "Keep all");
    assert_eq!(super::number_text("session.keep_exited", "5"), "5 sessions");
    assert_eq!(
        super::number_text("session.keep_exited", "7"),
        "7 sessions (custom)"
    );
    assert_eq!(
        applies_text("session.keep_exited", None),
        "Applies when the hub starts again."
    );

    let fake = Fake::new(vec![row(value("10"))]);
    let mut settings = at(&fake, "session.keep_exited");
    assert!(screen(&settings).contains("Keep exited"));
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "session.keep_exited", "20")]
    );
    assert_eq!(
        next_number_value(number_list("session.keep_exited").unwrap(), 20.0, true),
        Some(50.0)
    );

    let fake = Fake::new(vec![row(value("10"))]);
    refuse(&fake, "session.keep_exited is read-only");
    let mut settings = at(&fake, "session.keep_exited");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "session.keep_exited", "20")]
    );
    assert_eq!(settings.said, ["session.keep_exited is read-only"]);
}

#[test]
fn listed_numbers_step_from_exact_middle_values_in_both_directions() {
    let values = number_list("session.keep_exited").unwrap();
    assert_eq!(next_number_value(values, 10.0, true), Some(20.0));
    assert_eq!(next_number_value(values, 10.0, false), Some(5.0));
}

#[test]
fn a_custom_list_steps_to_the_nearest_value_in_each_direction_and_wraps() {
    let values = number_list("compaction.tokens").unwrap();
    assert_eq!(next_number_value(values, 300_000.0, true), Some(400_000.0));
    assert_eq!(next_number_value(values, 300_000.0, false), Some(200_000.0));
    assert_eq!(
        next_number_value(values, 100_000.0, false),
        Some(1_000_000.0)
    );
    assert_eq!(
        next_number_value(values, 1_000_000.0, true),
        Some(100_000.0)
    );
    assert_eq!(next_number_value(values, 0.0, true), Some(100_000.0));
    assert_eq!(next_number_value(values, 0.0, false), Some(1_000_000.0));
    assert_eq!(next_number_value(values, 99_999.0, true), Some(100_000.0));
    assert_eq!(next_number_value(values, 100_001.0, false), Some(100_000.0));
    assert_eq!(
        next_number_value(values, 2_000_000.0, false),
        Some(1_000_000.0)
    );
    let refresh = text_list("model_lists.refresh_after").unwrap();
    assert_eq!(next_text_value(refresh, "59m", true), Some("1h"));
    assert_eq!(next_text_value(refresh, "59m", false), Some("7d"));
    assert_eq!(next_text_value(refresh, "61m", true), Some("6h"));
    assert_eq!(next_text_value(refresh, "61m", false), Some("1h"));
    assert_eq!(next_text_value(refresh, "90m", true), Some("6h"));
    assert_eq!(next_text_value(refresh, "90m", false), Some("1h"));
    assert_eq!(next_text_value(refresh, "12h", true), Some("24h"));
    assert_eq!(next_text_value(refresh, "12h", false), Some("6h"));
    assert_eq!(next_text_value(refresh, "1h", false), Some("7d"));
    assert_eq!(next_text_value(refresh, "7d", true), Some("1h"));
    assert_eq!(next_text_value(refresh, "30m", false), Some("7d"));
    assert_eq!(refresh_millis("90m"), Some(5_400_000));
    assert_eq!(refresh_millis("1s"), Some(1_000));
}

#[test]
fn refresh_after_arrows_step_its_list_without_changing_tabs() {
    for (edit_key, next) in [(Edit::Right, "3d"), (Edit::Left, "12h")] {
        let fake = Fake::new(rows());
        let mut settings = at(&fake, "model_lists.refresh_after");
        let tab = settings.tab;
        edit(&mut settings, &fake, edit_key);
        assert_eq!(settings.tab, tab);
        assert_eq!(settings.tab, SettingsTab::Models);
        assert_eq!(
            writes(&fake),
            [w(
                Layer::Global,
                "model_lists.refresh_after",
                &format!("\"{next}\"")
            )]
        );
    }
}

#[test]
fn left_and_right_on_a_slider_step_the_value_without_changing_tabs() {
    for (edit_key, next) in [(Edit::Right, "31"), (Edit::Left, "29")] {
        let fake = Fake::new(with("tui.rail.width", value("30"), "project"));
        let mut settings = at(&fake, "tui.rail.width");
        let tab = settings.tab;
        edit(&mut settings, &fake, edit_key);
        assert_eq!(settings.tab, tab);
        assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", next)]);
    }
}

#[test]
fn sliders_step_by_their_range_and_enter_wraps_only_at_the_top() {
    let fake = Fake::new(with("tui.rail.width", value("30"), "project"));
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "31")]);

    let fake = Fake::new(with("tui.rail.width", value("40"), "project"));
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "10")]);

    let fake = Fake::new(with("tui.rail.width", value("10"), "project"));
    let mut settings = at(&fake, "tui.rail.width");
    edit(&mut settings, &fake, Edit::Left);
    assert!(writes(&fake).is_empty());
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "11")]);
}

#[test]
fn list_rows_step_forward_and_budget_no_limit_resets_the_override() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "compaction.tokens");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "compaction.tokens", "600000")]
    );

    let row = on(
        SettingsTab::Sessions,
        "Budget",
        "budget.usd",
        value("1"),
        "project",
        ANY,
        SettingControl::Number,
    );
    let fake = Fake::new(vec![row]);
    let mut settings = at(&fake, "budget.usd");
    edit(&mut settings, &fake, Edit::Left);
    assert!(writes(&fake).is_empty());
    assert_eq!(resets(&fake), [w(Layer::Project, "budget.usd", "1")]);
}

#[test]
fn a_custom_number_steps_to_the_next_list_entry_in_requested_direction() {
    let fake = Fake::new(with("compaction.tokens", value("300000"), "project"));
    let mut settings = at(&fake, "compaction.tokens");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "compaction.tokens", "400000")]
    );

    let fake = Fake::new(with("compaction.tokens", value("300000"), "project"));
    let mut settings = at(&fake, "compaction.tokens");
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "compaction.tokens", "200000")]
    );
}

#[test]
fn invalid_numeric_values_do_not_write_and_unset_controls_step_from_defaults() {
    for shown in [value("\"wide\""), value("\"soon\"")] {
        let key = if shown == value("\"wide\"") {
            "tui.rail.width"
        } else {
            "model_lists.refresh_after"
        };
        let fake = Fake::new(with(key, shown, "project"));
        let mut settings = at(&fake, key);
        one(&mut settings, &fake, Key::Enter);
        assert!(writes(&fake).is_empty(), "{key}");
    }

    let fake = Fake::new(with("tui.rail.width", Shown::Unset, "project"));
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "16")]);

    let fake = Fake::new(with("model_lists.refresh_after", Shown::Unset, "project"));
    let mut settings = at(&fake, "model_lists.refresh_after");
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "model_lists.refresh_after", "\"3d\"")]
    );
}

#[test]
fn budget_zero_is_custom_and_arrows_move_between_zero_and_unset() {
    let row = |shown| {
        on(
            SettingsTab::Sessions,
            "Budget",
            "budget.usd",
            shown,
            "project",
            ANY,
            SettingControl::Number,
        )
    };
    let fake = Fake::new(vec![row(value("0"))]);
    let mut settings = at(&fake, "budget.usd");
    assert_eq!(shown_value(settings.selected().unwrap()).0, "$0 (custom)");
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(writes(&fake), [w(Layer::Project, "budget.usd", "1")]);

    let fake = Fake::new(vec![row(value("0"))]);
    let mut settings = at(&fake, "budget.usd");
    edit(&mut settings, &fake, Edit::Left);
    assert!(writes(&fake).is_empty());
    assert_eq!(resets(&fake), [w(Layer::Project, "budget.usd", "0")]);

    let fake = Fake::new(vec![row(Shown::Unset)]);
    let mut settings = at(&fake, "budget.usd");
    assert_eq!(shown_value(settings.selected().unwrap()).0, "Default");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "budget.usd", "1")]);

    let fake = Fake::new(vec![row(Shown::Unset)]);
    let mut settings = at(&fake, "budget.usd");
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(writes(&fake), [w(Layer::Project, "budget.usd", "100")]);

    let fake = Fake::new(vec![on(
        SettingsTab::Sessions,
        "Compaction tokens",
        "compaction.tokens",
        value("0"),
        "project",
        ANY,
        SettingControl::Number,
    )]);
    let mut settings = at(&fake, "compaction.tokens");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Project, "compaction.tokens", "100000")]
    );
}

#[test]
fn slider_steps_pin_exact_minimum_and_maximum_boundaries() {
    let (minimum, maximum, step) = (0.5, 0.9, 0.05);
    assert!(
        (super::slider_step(maximum, minimum, maximum, step, false, false) - 0.85).abs() < 1e-12
    );
    assert_eq!(
        super::slider_step(maximum, minimum, maximum, step, true, false),
        maximum
    );
    assert_eq!(
        super::slider_step(maximum, minimum, maximum, step, true, true),
        minimum
    );
    assert_eq!(
        super::slider_step(minimum, minimum, maximum, step, false, false),
        minimum
    );
}

#[test]
fn slider_custom_values_step_to_nearest_tick_and_off_range_values_clamp() {
    assert_eq!(super::slider_step(0.525, 0.5, 0.9, 0.05, true, false), 0.55);
    assert_eq!(super::slider_step(0.525, 0.5, 0.9, 0.05, false, false), 0.5);
    assert_eq!(super::slider_step(0.95, 0.5, 0.9, 0.05, false, false), 0.9);
    assert_eq!(super::slider_step(0.95, 0.5, 0.9, 0.05, true, true), 0.5);
    assert_eq!(super::slider_step(0.45, 0.5, 0.9, 0.05, true, false), 0.5);
    assert_eq!(super::slider_step(0.95, 0.5, 0.9, 0.05, true, false), 0.9);
    assert_eq!(super::slider_step(0.95, 0.5, 0.9, 0.05, false, true), 0.9);
    assert_eq!(super::slider_step(0.5, 0.5, 0.9, 0.05, true, false), 0.55);
    assert_eq!(
        super::slider_step(0.5e-9, 0.0, 0.5e-9, 1.0, true, true),
        0.0
    );
    assert_eq!(
        super::slider_step(1.0e-9, 0.0, 1.0e-9, 1.0, true, true),
        1.0e-9
    );
    assert_eq!(
        super::slider_step(2.0e-9, 0.0, 2.0e-9, 1.0, true, true),
        2.0e-9
    );
    let stepped = (0..8).fold(0.5, |value, _| {
        super::slider_step(value, 0.5, 0.9, 0.05, true, false)
    });
    assert!((stepped - 0.9).abs() < 1e-12);
}

#[test]
fn a_width_row_counts_columns_for_its_width() {
    use SettingControl::Number;
    let mut all = with("tui.rail.width", value("15"), "project");
    all.push(on(
        SettingsTab::Appearance,
        "Panel width",
        "tui.panel.width",
        value("21"),
        "project",
        PERSON,
        Number,
    ));
    let fake = Fake::new(all);
    let settings = tab(&fake, SettingsTab::Appearance);
    let row_text = |terminal: usize, draw: u16, label: &str| {
        let area = Rect::new(0, 0, draw, 24);
        let mut buf = Buffer::empty(area);
        let frame = settings.frame(usize::from(draw), 24, None, terminal);
        render(&frame, area, &mut buf, &mut Vec::new());
        text_of(&buf, draw, 24)
            .lines()
            .map(str::to_owned)
            .find(|line| line.contains(label))
            .unwrap_or_else(|| panic!("no {label} row"))
    };
    // The rail's 15% reads 33 columns on 220 and 24 on 160.
    let rail = row_text(220, 220, "Rail width");
    assert!(rail.contains("15% of screen width · 33 columns"), "{rail}");
    let rail = row_text(160, 160, "Rail width");
    assert!(rail.contains("· 24 columns"), "{rail}");
    // The panel's 21% reads 46 columns on 220.
    let panel = row_text(220, 220, "Panel width");
    assert!(
        panel.contains("21% of screen width · 46 columns"),
        "{panel}"
    );
    // Rail attached: the suffix reads the terminal width while the
    // draw width keeps the room and the cut.
    let attached = row_text(220, 44, "Rail width");
    assert!(attached.contains("· 33 columns"), "{attached}");
    assert!(attached.contains('…'), "{attached}");
    // A terminal wider than u16 saturates instead of wrapping.
    let wide = row_text(100_000, 220, "Rail width");
    assert!(wide.contains("· 48 columns"), "{wide}");
}

#[test]
fn a_value_is_cut_to_the_room_left_after_the_label_and_the_layer_tag() {
    let long = "a".repeat(40);
    let fake = Fake::new(with("model", value(&format!("\"{long}\"")), "project"));
    let settings = at(&fake, "model");
    let layout = settings.layout(80, 24, None);
    let item = settings
        .items()
        .into_iter()
        .find(|item| matches!(item, super::Item::Key(at) if settings.rows[*at].key == "model"))
        .unwrap();
    // The gutter and the gap twice, the label column and the tag
    // "project" and its gap leave 13 columns of the 26 past the label.
    let width = layout.label_col + 26;
    // `at` focused this row above.
    let cells = settings.cells(item, &layout, width, false, true, width);
    assert_eq!(cells[1].0.chars().count(), 13, "{cells:?}");
    assert!(cells[1].0.ends_with('…'), "{cells:?}");
    assert_eq!(cells[2].0, "  project");
}

#[test]
fn a_list_pages_path_column_stops_eighteen_short_of_the_width() {
    let path = "p".repeat(40);
    let fake = Fake::new(extensions(&format!(
        r#"[{{"path":"{path}","required":true}}]"#
    )));
    let settings = page(&fake, "repository_extensions");
    let page = settings.page.as_ref().unwrap();
    let frame = settings.page_frame(page, 40, 24);
    // 40 columns less the gutter and sixteen, then the gap.
    assert_eq!(frame.rows[0][0].0.chars().count(), 22 + 2);
    assert_eq!(frame.rows[0][1].0, "[x] required");
}

#[test]
fn a_list_page_keeps_the_controls_rows_over_what_was_said() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    settings.said = (0..6).map(|at| format!("said {at}")).collect();
    let page = settings.page.as_ref().unwrap();
    // Nine rows less the title, the footer and three controls' rows.
    let frame = settings.page_frame(page, 80, 9);
    assert_eq!(frame.below, ["said 2", "said 3", "said 4", "said 5"]);
}

#[test]
fn a_list_page_scrolls_its_focus_into_the_rows_the_lines_below_leave() {
    let names: Vec<String> = (0..20).map(|at| format!("\"e{at:02}\"")).collect();
    let fake = Fake::new(with(
        "scoped_models",
        value(&format!("[{}]", names.join(","))),
        "global",
    ));
    let mut settings = page(&fake, "scoped_models");
    settings.page_select(20, &ctx(&fake));
    settings.said = vec!["note".to_owned()];
    // Ten rows less the title, the footer and one line below leave seven
    // rows: the last six names and "+ Add".
    let shown = drawn(&settings, None, 80, 10);
    assert!(shown.contains("e14"), "{shown}");
    assert!(!shown.contains("e13"), "{shown}");
    assert!(shown.contains("› + Add"), "{shown}");
}

#[test]
fn space_on_the_add_row_of_an_object_list_writes_nothing() {
    let fake = Fake::new(extensions(r#"[{"path":"a","required":false}]"#));
    let mut settings = page(&fake, "repository_extensions");
    one(&mut settings, &fake, Key::Down);
    one(&mut settings, &fake, Key::Char(' '));
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
}

#[test]
fn the_wheel_clears_what_was_said_only_when_the_focus_moves() {
    // Six rows leave too few for the tab's rows, so the wheel scrolls.
    let fake = Fake::new(rows());
    let small = sized(&fake, 80, 6);
    let mut settings = Settings::open(&small, Vec::new());
    settings.said = vec!["note".to_owned()];
    // Already on the first row: nothing moves.
    settings.wheel(true, &small);
    assert_eq!(settings.said, ["note"]);
    settings.wheel(false, &small);
    assert!(settings.said.is_empty());
}

#[test]
fn the_wheel_over_the_theme_choices_previews_only_when_the_selection_moves() {
    // Four rows show two of the three choices, so the wheel scrolls.
    let fake = Fake::new(rows());
    let small = sized(&fake, 80, 4);
    let mut settings = Settings::open_theme(&small, Vec::new());
    for _ in 0..names(&settings).len() {
        settings.wheel(true, &small);
    }
    // At the first choice, an upward wheel moves nothing.
    assert!(matches!(settings.wheel(true, &small), Act::Stay));
    assert!(matches!(settings.wheel(false, &small), Act::Theme(_)));
}

#[test]
fn a_field_and_a_page_print_without_their_draft() {
    let fake = editor_fake();
    let mut settings = at(&fake, "tui.editor");
    one(&mut settings, &fake, Key::Enter);
    typed(&mut settings, &fake, "sekret");
    let field = format!("{:?}", settings.mode);
    assert!(field.contains("Field"), "{field}");
    assert!(field.contains("tui.editor"), "{field}");
    assert!(field.contains("redacted"), "{field}");
    assert!(!field.contains("sekret"), "{field}");

    let fake = Fake::new(rows());
    let mut settings = page(&fake, "scoped_models");
    press(&mut settings, &fake, &[Key::Enter]);
    typed(&mut settings, &fake, "sekret");
    let page = format!("{:?}", settings.page);
    assert!(page.contains("Page"), "{page}");
    assert!(page.contains("scoped_models"), "{page}");
    assert!(page.contains("PageField"), "{page}");
    assert!(page.contains("redacted"), "{page}");
    assert!(!page.contains("sekret"), "{page}");
}

#[test]
fn a_narrow_tab_bar_drops_tabs_from_the_left_and_keeps_the_open_one() {
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Advanced);
    let bar = |width: usize| -> String {
        settings
            .tab_bar(width)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    };
    assert!(bar(80).starts_with(" Appearance "), "{}", bar(80));
    // " Sessions " and " Advanced " fit in 21 columns, with the gap.
    assert_eq!(bar(21), " Sessions   Advanced ");
    // Narrower than the open tab alone, the open tab stays.
    assert_eq!(bar(3), " Advanced ");
}

// The cross-tab search (`docs/tui.md`, "Swapped views", `/settings`,
// "Search").

/// Opens a search for `query` from Appearance: `/`, then each of its
/// characters.
fn searched(fake: &Fake, query: &str) -> Settings {
    let mut settings = open(fake);
    one(&mut settings, fake, Key::Char('/'));
    typed(&mut settings, fake, query);
    settings
}

/// The open query's text.
fn query_of(settings: &Settings) -> String {
    settings
        .search
        .as_ref()
        .map(|search| search.query.expand())
        .unwrap_or_else(|| panic!("no search open"))
}

/// Asserts `name` draws dim as a tab annotation, ending at the frame's
/// right edge.
fn assert_annotation(settings: &Settings, width: u16, height: u16, name: &str) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(
        usize::from(width),
        usize::from(height),
        None,
        usize::from(width),
    );
    render(&frame, area, &mut buf, &mut Vec::new());
    let found = (0..height).find_map(|y| {
        let line: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        line.find(name)
            .map(|byte| (u16::try_from(line[..byte].chars().count()).unwrap(), y))
    });
    let (x, y) = found.unwrap_or_else(|| panic!("no annotation {name:?}"));
    assert!(
        buf[(x, y)].modifier.contains(Modifier::DIM),
        "{name} is not dim"
    );
    assert_eq!(
        usize::from(x) + name.chars().count(),
        usize::from(width),
        "{name} is not at the right"
    );
}

#[test]
fn settings_search_matches_label_and_path_across_tabs() {
    let fake = Fake::new(rows());
    // `reviewer` matches the label, `reviewer.model` the key path: both
    // land on the Permissions row from Appearance.
    for query in ["reviewer", "reviewer.model"] {
        let settings = searched(&fake, query);
        let items = settings.items();
        assert_eq!(items.len(), 1, "{query}");
        assert!(
            matches!(&items[0], super::Item::Key(at) if settings.rows[*at].key == "reviewer.model"),
            "{query}: {items:?}"
        );
        assert_eq!(
            settings.focus,
            Some(super::Focus::Key("reviewer.model".to_owned())),
            "{query}"
        );
        // The search holds the opening tab while its rows span tabs.
        assert_eq!(settings.tab, SettingsTab::Appearance, "{query}");
        let shown = screen(&settings);
        assert_eq!(
            row_of(&shown, "Reviewer model"),
            "› Reviewer model Default › Permissions",
            "{query}"
        );
    }
    expect(
        &screen(&searched(&fake, "reviewer")),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
|/ reviewer
|
|› Reviewer model  Default ›                                          Permissions
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|About reviewer.model.
|reviewer.model · not set · changing reviewer.model needs a reload
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn settings_search_matches_label_only_and_path_only_needles() {
    let fake = Fake::new(rows());
    // `terminal` is in the label alone; `attention.bell` in the path
    // alone. Either side of the match going missing loses its needle.
    for query in ["terminal", "attention.bell"] {
        let settings = searched(&fake, query);
        let items = settings.items();
        assert_eq!(items.len(), 1, "{query}");
        assert!(
            matches!(&items[0], super::Item::Key(at) if settings.rows[*at].key == "tui.attention.bell"),
            "{query}: {items:?}"
        );
        assert_eq!(
            settings.focus,
            Some(super::Focus::Key("tui.attention.bell".to_owned())),
            "{query}"
        );
    }
}

#[test]
fn settings_search_matches_letter_activation_and_slash_empty_query() {
    let fake = Fake::new(rows());
    let mut settings = open(&fake);
    one(&mut settings, &fake, Key::Char('r'));
    assert!(settings.search.is_some());
    assert_eq!(query_of(&settings), "r");
    // A digit or other punctuation opens nothing.
    for ch in ['3', '.', '-'] {
        let mut plain = open(&fake);
        one(&mut plain, &fake, Key::Char(ch));
        assert!(plain.search.is_none(), "{ch}");
    }
    // `/` opens an empty query over every eligible row, in tab order:
    // every tabbed key and every screen row, and no hidden instance.
    let mut settings = tab(&fake, SettingsTab::Sessions);
    one(&mut settings, &fake, Key::Char('/'));
    assert_eq!(query_of(&settings), "");
    assert_eq!(settings.items().len(), 21);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.hover".to_owned()))
    );
    // The opening tab stays put while the results span tabs.
    assert_eq!(settings.tab, SettingsTab::Sessions);
    let shown = screen(&settings);
    assert!(shown.lines().any(|line| line == "/"), "{shown}");
    // No tab bar: its names only return as dim annotations, one per row.
    assert!(
        !shown
            .lines()
            .any(|line| line.contains("Notifications   Models")),
        "{shown}"
    );
    assert!(shown.contains("Permissions"), "{shown}");
}

#[test]
fn emptied_query_closes_typed_search() {
    let fake = Fake::new(rows());
    let mut settings = searched(&fake, "the");
    assert!(settings.search.is_some());

    press(
        &mut settings,
        &fake,
        &[Key::Backspace, Key::Backspace, Key::Backspace],
    );

    assert_search_closed(&settings);
}

#[test]
fn emptied_query_closes_seeded_search() {
    let fake = Fake::new(rows());
    let mut settings = open(&fake);
    one(&mut settings, &fake, Key::Char('r'));
    assert_eq!(query_of(&settings), "r");

    one(&mut settings, &fake, Key::Backspace);

    assert_search_closed(&settings);
}

fn assert_search_closed(settings: &Settings) {
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Appearance);
    let shown = screen(settings);
    assert!(shown.contains("Appearance   Notifications"), "{shown}");
    assert!(shown.contains("› Hover"), "{shown}");
}

#[test]
fn settings_search_matches_case_insensitively_and_by_substring_with_backspace_and_paste() {
    let fake = Fake::new(rows());
    for query in ["REVIEWER", "eviewer"] {
        let settings = searched(&fake, query);
        assert_eq!(
            settings.focus,
            Some(super::Focus::Key("reviewer.model".to_owned())),
            "{query}"
        );
    }
    // Backspace shortens the query and restores the match.
    let mut settings = searched(&fake, "reviewerx");
    assert!(settings.items().is_empty());
    one(&mut settings, &fake, Key::Backspace);
    assert_eq!(query_of(&settings), "reviewer");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("reviewer.model".to_owned()))
    );
    // Backspace on an empty query keeps the search open.
    let mut settings = open(&fake);
    one(&mut settings, &fake, Key::Char('/'));
    one(&mut settings, &fake, Key::Backspace);
    assert_eq!(query_of(&settings), "");
    assert_eq!(settings.items().len(), 21);
    // A paste lands as one line: the newline and the tab go.
    let mut settings = open(&fake);
    one(&mut settings, &fake, Key::Char('/'));
    edit(
        &mut settings,
        &fake,
        Edit::Paste("reviewer.mo\n\tdel".to_owned()),
    );
    assert_eq!(query_of(&settings), "reviewer.model");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("reviewer.model".to_owned()))
    );
}

#[test]
fn settings_search_matches_screen_rows_by_label_and_covered_path() {
    let fake = Fake::new(rows());
    // `skills` matches the label; `skills.disabled` the covered path.
    // Either way the Skills screen draws once, with no hidden key row
    // doubling it.
    for query in ["skills", "skills.disabled"] {
        let settings = searched(&fake, query);
        assert_eq!(
            settings.items(),
            vec![super::Item::Screen(super::Screen::Skills)],
            "{query}"
        );
        assert_eq!(
            settings.focus,
            Some(super::Focus::Screen(super::Screen::Skills)),
            "{query}"
        );
        let shown = screen(&settings);
        assert!(
            shown.lines().any(|line| line.contains("Open /skills")),
            "{query}:\n{shown}"
        );
    }
    assert_annotation(&searched(&fake, "skills"), 80, 24, "Sessions");
    // Rules has no covered key path: only its label matches.
    let settings = searched(&fake, "rules");
    assert_eq!(
        settings.items(),
        vec![super::Item::Screen(super::Screen::Rules)]
    );
    assert_annotation(&settings, 80, 24, "Permissions");
}

#[test]
fn settings_search_matches_zero_results_says_so_and_takes_no_row() {
    let fake = Fake::new(rows());
    let mut settings = searched(&fake, "zzz");
    assert!(settings.items().is_empty());
    assert_eq!(settings.focus, None);
    let shown = screen(&settings);
    assert!(
        shown.contains("No settings match \u{201c}zzz\u{201d}."),
        "{shown}"
    );
    assert!(shown.lines().any(|line| line == "/ zzz"), "{shown}");
    assert!(!shown.contains("Notifications"), "{shown}");
    // No row means no write, no reset, no request and no screen.
    for key in [Key::Enter, Key::Char(' '), Key::Tab] {
        assert!(matches!(one(&mut settings, &fake, key), Act::Stay));
    }
    assert!(matches!(
        edit(&mut settings, &fake, Edit::Delete),
        Act::Stay
    ));
    assert!(fake.writes().is_empty());
    assert!(resets(&fake).is_empty());
    assert!(settings.take_request().is_none());
    assert!(matches!(settings.mode, super::Mode::Rows));
    // With no selected key Ctrl+G keeps its global-file fallback.
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(
        matches!(&act, Act::Open(path) if *path == fake.global),
        "{act:?}"
    );
    expect(
        &screen(&searched(&fake, "zzz")),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
|/ zzz
|
|  No settings match “zzz”.
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
}

#[test]
fn settings_search_matches_annotate_each_match_dim_at_the_right() {
    let fake = Fake::new(rows());
    let settings = searched(&fake, "sandbox");
    assert_eq!(settings.items().len(), 2);
    // The value keeps its layer tag beside the annotation.
    let shown = screen(&settings);
    assert!(
        shown.contains("repository") && shown.contains("Permissions"),
        "{shown}"
    );
    assert_eq!(
        shown
            .lines()
            .filter(|line| line.ends_with("Permissions"))
            .count(),
        2,
        "{shown}"
    );
    assert_annotation(&settings, 80, 24, "Permissions");
}

#[test]
fn settings_search_matches_keep_layer_tag_and_clip_safely_when_narrow() {
    let fake = Fake::new(rows());
    let settings = searched(&fake, "sandbox");
    // At 60 columns the value, its layer tag and the annotation fit.
    let shown = drawn(&settings, None, 60, 24);
    assert!(
        shown.contains("repository") && shown.lines().any(|line| line.ends_with("Permissions")),
        "{shown}"
    );
    assert_annotation(&settings, 60, 24, "Permissions");
    // At 40 the annotation is cut, but the frame still draws and the
    // focused row keeps its mark and identity.
    let narrow = drawn(&settings, None, 40, 24);
    assert_eq!(narrow.lines().count(), 24, "{narrow}");
    assert!(narrow.lines().any(|line| line.starts_with("›")), "{narrow}");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.allow_write".to_owned()))
    );
    // Wide characters in the query never break the no-match line.
    let mut wide = open(&fake);
    one(&mut wide, &fake, Key::Char('/'));
    typed(&mut wide, &fake, "railé");
    assert!(wide.items().is_empty());
    let shown = screen(&wide);
    assert_eq!(shown.lines().count(), 24, "{shown}");
    assert!(
        shown.contains("No settings match \u{201c}railé\u{201d}."),
        "{shown}"
    );
    one(&mut wide, &fake, Key::Backspace);
    assert_eq!(
        wide.focus,
        Some(super::Focus::Key("tui.rail.width".to_owned()))
    );
}

#[test]
fn settings_search_matches_narrow_annotation_cuts_the_value_short() {
    let fake = Fake::new(rows());
    // At 40 columns the value is cut short of the tab annotation, keeping
    // the layer tag whole: the exact cut pins the annotation's reserved
    // room, so narrowing that reserve fails here.
    expect(
        &drawn(&searched(&fake, "sandbox"), None, 40, 24),
        r#"
| Settings  /settings         Esc returns
|Changes save automatically
|
|/ sandbox
|
|› Sandbox writable paths  …  Permissions
|
|  Sandbox                 …  repositoryP
|
|
|
|
|
|
|
|
|
|
|
|
|
|About sandbox.allow_write.
|sandbox.allow_write · not set · changing
|←→ tab · / search · Enter change · Tab l
"#,
    );
}

#[test]
fn settings_search_matches_draw_in_a_short_frame() {
    let fake = Fake::new(rows());
    let settings = searched(&fake, "reviewer");
    let shown = drawn(&settings, None, 80, 12);
    assert!(shown.lines().any(|line| line == "/ reviewer"), "{shown}");
    assert!(!shown.contains("Notifications"), "{shown}");
    assert!(shown.contains("Reviewer model"), "{shown}");
    assert!(shown.contains("Permissions"), "{shown}");
}

// The ruled Escape transitions (`docs/tui.md`, "Swapped views",
// `/settings`, "Search"): with a match the focus stays on it in its own
// tab; with no match it goes back to the row focused when the search
// opened. The next Esc returns.

/// The fixture rows without `key`.
fn without(key: &str) -> Vec<SettingRow> {
    rows().into_iter().filter(|row| row.key != key).collect()
}

#[test]
fn settings_search_escape_keeps_the_match_in_its_tab_and_closes_next() {
    let fake = Fake::new(rows());
    // `tui` matches across Appearance and Notifications: move past the
    // first result, so keeping it cannot pass.
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "tui");
    press(&mut settings, &fake, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.attention.bell".to_owned()))
    );
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
|/ tui
|
|  Hover          [x] On                                               Appearance
|
|  Rail width     ▕█████░░░▏ 30% of screen wid… · 24 columns  project  Appearance
|
|  Theme          Default                                              Appearance
|
|› Terminal bell  [ ] Off                                           Notifications
|
|
|
|
|
|
|
|
|
|About tui.attention.bell.
|tui.attention.bell · using the default · Applies when the terminal starts again.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    // The first Esc clears but stays open, on the match in its own tab.
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Notifications);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.attention.bell".to_owned()))
    );
    expect(
        &screen(&settings),
        r#"
| Settings  /settings                                                 Esc returns
|Changes save automatically
|
| Appearance   Notifications   Models   Permissions   Sessions   Advanced
|
|› Terminal bell  [ ] Off
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|
|About tui.attention.bell.
|tui.attention.bell · using the default · Applies when the terminal starts again.
|←→ tab · / search · Enter change · Tab layer · Delete reset
"#,
    );
    // The next Esc returns.
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn settings_search_escape_with_no_match_restores_the_origin_row() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    let origin = screen(&settings);
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "zzz");
    assert_eq!(settings.focus, None);
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.rail.width".to_owned()))
    );
    assert_eq!(screen(&settings), origin);
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn settings_search_escape_empty_query_keeps_the_match() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    // Even with an empty query, a displayed focused match stays on its
    // own tab: only "with no match" goes back (`docs/tui.md`, "Swapped
    // views", `/settings`, "Search").
    press(
        &mut settings,
        &fake,
        &[Key::Down, Key::Down, Key::Down, Key::Down],
    );
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.attention.bell".to_owned()))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Notifications);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.attention.bell".to_owned()))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn settings_search_escape_screen_match_stays_on_its_tab() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "skills");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(super::Screen::Skills))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Sessions);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(super::Screen::Skills))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn settings_search_escape_reopen_empty_query_keeps_the_first_match() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "skills");
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert_eq!(settings.tab, SettingsTab::Sessions);
    // Opening again starts an empty query over every row, so the first
    // result takes the focus; Esc stays on that match in its own tab
    // (`docs/tui.md`, "Swapped views", `/settings`, "Search").
    one(&mut settings, &fake, Key::Char('/'));
    assert_eq!(query_of(&settings), "");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.hover".to_owned()))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.hover".to_owned()))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

#[test]
fn settings_search_escape_reread_keeps_result_and_falls_back_without_it() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "reviewer");
    // A reread keeps the focused result and the query.
    settings.reread(&ctx(&fake));
    assert_eq!(query_of(&settings), "reviewer");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("reviewer.model".to_owned()))
    );
    // A reread that removes the focused result leaves no match: Esc
    // restores the origin.
    *support::lock(&fake.rows) = Ok(without("reviewer.model"));
    settings.reread(&ctx(&fake));
    assert!(settings.items().is_empty());
    assert_eq!(settings.focus, None);
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.rail.width".to_owned()))
    );
    // A reread that removes the origin row instead falls back to the
    // first remaining row of the origin tab, never a stale index.
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "zzz");
    *support::lock(&fake.rows) = Ok(without("tui.rail.width"));
    settings.reread(&ctx(&fake));
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.hover".to_owned()))
    );
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Close));
}

// Saving, editing and pointing from a search (`docs/tui.md`, "Swapped
// views", `/settings`, "Search"): every control keeps the contract the
// tabbed rows have, and the pointer, wheel and reread act on the rows
// the search shows.

#[test]
fn settings_search_controls_save_through_the_existing_control_and_layer() {
    let fake = Fake::new(rows());
    // A filtered boolean flips in the layer its value comes from: unset
    // lands on global first.
    let mut settings = searched(&fake, "hover");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.hover", "false")]);
    assert_eq!(query_of(&settings), "hover");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.hover".to_owned()))
    );
    assert_eq!(settings.said, ["Saved to global config.".to_owned()]);
    // A filtered fixed row steps in the schema's order, as on its tab.
    let mut settings = searched(&fake, "thinking");
    one(&mut settings, &fake, Key::Char(' '));
    assert_eq!(
        writes(&fake),
        [
            w(Layer::Global, "tui.hover", "false"),
            w(Layer::Global, "thinking", "\"off\"")
        ]
    );
    assert_eq!(query_of(&settings), "thinking");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("thinking".to_owned()))
    );
}

#[test]
fn settings_search_controls_field_owns_its_keys_until_it_cancels() {
    let mut all = rows();
    all.push(on(
        SettingsTab::Appearance,
        "Editor",
        "tui.editor",
        value("\"vim\""),
        "project",
        PERSON,
        SettingControl::Text,
    ));
    let fake = Fake::new(all);
    let mut settings = searched(&fake, "editor");
    one(&mut settings, &fake, Key::Enter);
    assert!(matches!(settings.mode, super::Mode::Field(_)));
    // Letters, a slash and a paste land in the field, never the query.
    typed(&mut settings, &fake, "ab");
    one(&mut settings, &fake, Key::Char('/'));
    edit(&mut settings, &fake, Edit::Paste("3".to_owned()));
    let super::Mode::Field(field) = &settings.mode else {
        panic!("no field");
    };
    assert_eq!(field.draft.expand(), "vimab/3");
    assert_eq!(query_of(&settings), "editor");
    assert!(fake.writes().is_empty());
    // The field's own Esc writes nothing and keeps the search: the next
    // Esc clears it onto the match, as the ruling says.
    one(&mut settings, &fake, Key::Esc);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(query_of(&settings), "editor");
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.editor".to_owned()))
    );
}

#[test]
fn settings_search_controls_list_page_returns_to_the_searched_row() {
    let fake = Fake::new(rows());
    let mut settings = searched(&fake, "sandbox");
    one(&mut settings, &fake, Key::Enter);
    assert!(
        settings
            .page
            .as_ref()
            .is_some_and(|page| page.key == "sandbox.allow_write")
    );
    // Esc closes the page, not the search: the row stays focused.
    one(&mut settings, &fake, Key::Esc);
    assert!(settings.page.is_none());
    assert_eq!(query_of(&settings), "sandbox");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.allow_write".to_owned()))
    );
    // A nested field's Esc steps come before the search's: field, then
    // page, then the search itself.
    one(&mut settings, &fake, Key::Enter);
    one(&mut settings, &fake, Key::Enter);
    assert!(
        settings
            .page
            .as_ref()
            .is_some_and(|page| page.field.is_some())
    );
    typed(&mut settings, &fake, "p");
    one(&mut settings, &fake, Key::Esc);
    assert!(
        settings
            .page
            .as_ref()
            .is_some_and(|page| page.field.is_none())
    );
    one(&mut settings, &fake, Key::Esc);
    assert!(settings.page.is_none());
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert_eq!(settings.tab, SettingsTab::Permissions);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.allow_write".to_owned()))
    );
    assert!(fake.writes().is_empty());
}

#[test]
fn settings_search_controls_model_picker_saves_to_the_captured_row() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "reviewer");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(settings.take_request(), Some(Request::Model));
    assert_eq!(
        settings.awaiting,
        Some(("reviewer.model".to_owned(), Layer::Global))
    );
    assert!(fake.writes().is_empty());
    assert!(settings.choose_model("a/b", &ctx(&fake)));
    // The reference saves to the captured key and layer, never the
    // origin row.
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "reviewer.model", "\"a/b\"")]
    );
    assert!(!settings.awaiting_model());
    assert_eq!(query_of(&settings), "reviewer");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("reviewer.model".to_owned()))
    );
}

#[test]
fn settings_search_controls_dash_c_stays_read_only_and_delete_resets_the_match() {
    let fake = Fake::new(with("tui.hover", value("\"true\""), "-c"));
    let mut settings = open(&fake);
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "hover");
    let shown = screen(&settings);
    assert!(shown.contains("set by -c for this run"), "{shown}");
    for key in [Key::Enter, Key::Char(' '), Key::Tab] {
        assert!(matches!(one(&mut settings, &fake, key), Act::Stay));
    }
    assert!(matches!(
        edit(&mut settings, &fake, Edit::Delete),
        Act::Stay
    ));
    assert!(fake.writes().is_empty());
    assert!(resets(&fake).is_empty());
    assert!(settings.take_request().is_none());
    assert!(!settings.awaiting_model());
    // Delete resets the focused editable match, not the origin row.
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.hover");
    one(&mut settings, &fake, Key::Char('/'));
    typed(&mut settings, &fake, "rail");
    assert!(matches!(
        edit(&mut settings, &fake, Edit::Delete),
        Act::Stay
    ));
    assert_eq!(
        resets(&fake),
        [(Layer::Project, "tui.rail.width".to_owned(), "30".to_owned())]
    );
    assert!(fake.writes().is_empty());
    assert_eq!(query_of(&settings), "rail");
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.rail.width".to_owned()))
    );
}

#[test]
fn settings_search_controls_left_and_right_edit_the_query_not_the_tabs() {
    let fake = Fake::new(rows());
    let mut settings = searched(&fake, "rail");
    // With a search open Left and Right move in the query: the tab stays
    // put and the query text is untouched by the moves alone.
    edit(&mut settings, &fake, Edit::Right);
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    assert!(settings.search.is_some());
    assert_eq!(query_of(&settings), "rail");
    // The cursor they move is real: typing lands where it sits.
    edit(&mut settings, &fake, Edit::Left);
    one(&mut settings, &fake, Key::Char('x'));
    assert_eq!(query_of(&settings), "raxil");
}

#[test]
fn settings_search_controls_pointer_and_wheel_follow_the_displayed_rows() {
    use crate::mouse::TargetId;
    let fake = Fake::new(rows());
    let mut settings = searched(&fake, "sandbox");
    // Up and Down move by the displayed matching identity.
    one(&mut settings, &fake, Key::Down);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.enabled".to_owned()))
    );
    one(&mut settings, &fake, Key::Up);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.allow_write".to_owned()))
    );
    // A renderer-produced row click selects that row's identity: the
    // frame's row gives the click, and the renderer must carry a target
    // for it.
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None, 80);
    let mut targets = Vec::new();
    render(&frame, area, &mut buf, &mut targets);
    let at = frame
        .rows
        .iter()
        .position(|cells| cells.iter().any(|(text, _, _)| text.contains("[x] On")))
        .expect("no Sandbox row");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::View(Spot::Row(at))),
        "no click target on the Sandbox row"
    );
    settings.click(Spot::Row(at), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("sandbox.enabled".to_owned()))
    );
    // With more matches than fit, the wheel scrolls onto another
    // displayed row: bounded, so a stuck wheel fails instead of hanging.
    let mut tall = open(&fake);
    one(&mut tall, &fake, Key::Char('/'));
    let before = tall.focus.clone();
    for _ in 0..40 {
        tall.wheel(false, &ctx(&fake));
        if tall.focus != before {
            break;
        }
    }
    assert_ne!(tall.focus, before);
    assert!(
        tall.items()
            .iter()
            .any(|item| tall.focus_of(*item) == tall.focus)
    );
    // A reread and a new size leave no stale target: the same identity
    // stays focused and drawn with its mark.
    settings.reread(&ctx(&fake));
    assert_eq!(query_of(&settings), "sandbox");
    let shown = drawn(&settings, None, 60, 10);
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with("›") && line.contains("[x] On")),
        "{shown}"
    );
    assert!(
        shown
            .lines()
            .any(|line| !line.starts_with("›") && line.contains("writable")),
        "{shown}"
    );
}

#[test]
fn settings_search_controls_ctrl_g_and_plain_keys_keep_their_routes() {
    let fake = Fake::new(rows());
    // Ctrl+G over a result opens that row's own file.
    let mut settings = searched(&fake, "rail");
    let act = one(&mut settings, &fake, Key::CtrlG);
    assert!(
        matches!(&act, Act::OpenLocal(Some(path)) if *path == file(Layer::Project)),
        "{act:?}"
    );
    // Clearing restores ordinary keys: after focusing a non-numeric row,
    // Left and Right switch tabs again.
    assert!(matches!(one(&mut settings, &fake, Key::Esc), Act::Stay));
    assert!(settings.search.is_none());
    assert!(focus_non_numeric(&mut settings, &fake));
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(settings.tab, SettingsTab::Notifications);
    // The theme choices still open outside any search.
    let themed = Settings::open_theme(&ctx(&fake), Vec::new());
    assert!(matches!(themed.mode, super::Mode::Choices { .. }));
}
