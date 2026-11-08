//! Tests for `/settings`: the tabs, the control each key's type gives it,
//! where a change saves, the list sub-page, and when a write applies
//! (`docs/tui.md`, "Swapped views", `/settings`; `docs/configuration.md`,
//! "When Fiber reads configuration"). Screens are drawn through the real
//! frame renderer and compared whole.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{
    Act, Applies, Ctx, Request, SCREEN_ROWS, Screen, ScreenRow, Settings, applies, applies_text,
    as_typed, layers,
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
            "Handoff tokens",
            "handoff.tokens",
            value("400000"),
            "default",
            ANY,
            Number,
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
fn tab(fake: &Fake, tab: SettingsTab) -> Settings {
    let mut settings = open(fake);
    while settings.tab != tab {
        settings.edit_key(Edit::Right, &ctx(fake));
    }
    settings
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
    let mut settings = tab(fake, tab_of);
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
    let frame = settings.frame(usize::from(width), usize::from(height), usage);
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
    if let Ok(path) = std::env::var("FIBER_BLESS") {
        use std::io::Write;
        let line = std::panic::Location::caller().line();
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(out, "@@@ {line}\n{screen}\n@@@@").unwrap();
        return;
    }
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
|  Rail width    30  project
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
|  Rail width    30  project
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
|  Model list refresh  24h
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
|› Reviewer model          Not set ›
|
|  Sandbox writable paths  none ›
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
|› Handoff tokens         400000
|
|  Repository extensions  none ›
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
|About handoff.tokens.
|handoff.tokens · using the default · changing handoff.tokens needs a reload
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
|  Hub port           Not set
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
fn the_open_tab_is_bold_on_the_surface_tint_and_the_footer_keys_are_bold() {
    use crate::theme::Role;
    let fake = Fake::new(rows());
    let settings = tab(&fake, SettingsTab::Models);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let frame = settings.frame(80, 24, None);
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
    let open = cell_at("Models", bar);
    assert!(open.modifier.contains(Modifier::BOLD));
    assert_eq!(open.bg, Role::Surface.color());
    // The other tabs are plain: neither bold nor tinted.
    for name in ["Appearance", "Notifications", "Permissions", "Advanced"] {
        let cell = cell_at(name, bar);
        assert!(!cell.modifier.contains(Modifier::BOLD), "{name}");
        assert_ne!(cell.bg, Role::Surface.color(), "{name}");
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

#[test]
fn left_and_right_move_between_tabs_and_stop_at_the_ends() {
    let fake = Fake::new(rows());
    let mut settings = open(&fake);
    edit(&mut settings, &fake, Edit::Left);
    assert_eq!(settings.tab, SettingsTab::Appearance);
    for want in &SettingsTab::ALL[1..] {
        edit(&mut settings, &fake, Edit::Right);
        assert_eq!(settings.tab, *want);
    }
    edit(&mut settings, &fake, Edit::Right);
    assert_eq!(settings.tab, SettingsTab::Advanced);
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
    // Models has 4 keys and one screen row: 5 rows need 9 body lines.
    let tall = drawn(&settings, None, 80, 24);
    let lines: Vec<&str> = tall.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("Default model"))
        .unwrap();
    assert_eq!(lines.get(at + 1), Some(&""), "{tall}");
    // Below 18 rows the top rows drop their blank lines: two fixed rows,
    // two lines under the list. A body of exactly nine lines is spaced.
    let at_edge = settings.layout(80, 2 + 2 + 2 + 9, None);
    assert_eq!(at_edge.body, 9);
    assert!(at_edge.lines.contains(&super::Line::Blank));
    // One line fewer and the rows pack.
    let packed = settings.layout(80, 2 + 2 + 2 + 8, None);
    assert_eq!(packed.body, 8);
    assert!(!packed.lines.contains(&super::Line::Blank));
    assert_eq!(packed.lines.len(), 5);
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
fn a_number_opens_a_field_in_the_value_place_and_enter_saves_it_as_typed() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    // The field holds the value and sits where the value was.
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
|› Rail width    30
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
|About tui.rail.width.
|tui.rail.width · set in project · Applies when the terminal starts again.
|Enter save · Esc cancel
"#,
    );
    typed(&mut settings, &fake, "5");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Project, "tui.rail.width", "305")]);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(settings.said, ["Saved to project config."]);
}

#[test]
fn free_text_is_saved_as_a_string_whatever_it_looks_like() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model_lists.refresh_after");
    one(&mut settings, &fake, Key::Enter);
    press(
        &mut settings,
        &fake,
        &[Key::Backspace, Key::Backspace, Key::Backspace],
    );
    typed(&mut settings, &fake, "7d");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(
        writes(&fake),
        [w(Layer::Global, "model_lists.refresh_after", "\"7d\"")]
    );
    // Text that parses as JSON is still text.
    let mut again = at(&fake, "model_lists.refresh_after");
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
        Some(&w(Layer::Global, "model_lists.refresh_after", "\"12\""))
    );
}

#[test]
fn a_refused_value_keeps_the_field_and_the_draft_with_the_error_under_it() {
    let fake = Fake::new(rows());
    refuse(&fake, "tui.rail.width must be a number");
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    typed(&mut settings, &fake, "x");
    one(&mut settings, &fake, Key::Enter);
    let super::Mode::Field(field) = &settings.mode else {
        panic!("the field closed on a refused value");
    };
    assert_eq!(field.draft.expand(), "30x");
    assert_eq!(settings.said, ["tui.rail.width must be a number"]);
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
|› Rail width    30x
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
|About tui.rail.width.
|tui.rail.width · set in project · Applies when the terminal starts again.
|tui.rail.width must be a number
|Enter save · Esc cancel
"#,
    );
    // Esc closes the field and writes nothing more.
    let before = writes(&fake).len();
    one(&mut settings, &fake, Key::Esc);
    assert!(matches!(settings.mode, super::Mode::Rows));
    assert_eq!(writes(&fake).len(), before);
    assert!(settings.said.is_empty());
}

#[test]
fn esc_in_a_field_cancels_with_no_write() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
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
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "model_lists.refresh_after");
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
    assert_eq!(settings.tab, SettingsTab::Models);
    assert!(resets(&fake).is_empty());
    let super::Mode::Field(field) = &settings.mode else {
        panic!("no field");
    };
    assert_eq!(field.draft.expand(), "abcx日");
}

#[test]
fn the_wheel_and_clicks_do_not_retarget_an_open_field() {
    let fake = Fake::new(rows());
    let mut settings = at(&fake, "tui.rail.width");
    one(&mut settings, &fake, Key::Enter);
    settings.wheel(false, &ctx(&fake));
    settings.click(Spot::Row(0), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("tui.rail.width".to_owned()))
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
    assert_eq!(
        settings.take_request(),
        Some(Request::Model {
            key: "model".to_owned(),
            layer: Layer::Global
        })
    );
    assert!(settings.awaiting_model());
    // Nothing is written until the picker chooses.
    assert!(fake.writes().is_empty());
    // Space asks the same, on a key whose value is unset.
    let mut reviewer = at(&fake, "reviewer.model");
    one(&mut reviewer, &fake, Key::Char(' '));
    assert_eq!(
        reviewer.take_request(),
        Some(Request::Model {
            key: "reviewer.model".to_owned(),
            layer: Layer::Global
        })
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
        "› Rail width 30 project"
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
    typed(&mut settings, &fake, "1");
    one(&mut settings, &fake, Key::Enter);
    assert_eq!(writes(&fake), [w(Layer::Global, "tui.rail.width", "301")]);
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

#[test]
fn up_and_down_move_the_focus_by_row_and_a_move_clears_what_was_said() {
    let fake = Fake::new(rows());
    let mut settings = tab(&fake, SettingsTab::Models);
    one(&mut settings, &fake, Key::Down);
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.refresh_after".to_owned()))
    );
    settings.said = vec!["Saved.".to_owned()];
    one(&mut settings, &fake, Key::Down);
    assert!(settings.said.is_empty());
    // Past the last row the focus stays; past the first too.
    for _ in 0..10 {
        one(&mut settings, &fake, Key::Down);
    }
    assert_eq!(
        settings.focus,
        Some(super::Focus::Screen(Screen::Credentials))
    );
    for _ in 0..10 {
        one(&mut settings, &fake, Key::Up);
    }
    assert_eq!(settings.focus, Some(super::Focus::Key("model".to_owned())));
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
        Some(super::Focus::Key("model_lists.refresh_after".to_owned()))
    );
    // The line between two rows is not a row.
    settings.click(Spot::Row(second + 1), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.refresh_after".to_owned()))
    );
    settings.click(Spot::Row(500), &ctx(&fake));
    assert_eq!(
        settings.focus,
        Some(super::Focus::Key("model_lists.refresh_after".to_owned()))
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
    render(&settings.frame(80, 24, None), area, &mut buf, &mut targets);
    let shown = text_of(&buf, 80, 24);
    for label in ["Default model", "Scoped models", "Credentials"] {
        let y =
            u16::try_from(shown.lines().position(|line| line.contains(label)).unwrap()).unwrap();
        let spot = targets
            .iter()
            .filter(|target| target.rect.y == y)
            .find_map(|target| match target.id {
                crate::mouse::TargetId::View(Spot::Row(at)) => Some(at),
                _ => None,
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
    assert_eq!(settings.frame(80, 24, None).title, "Scoped models");
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
    let frame = settings.frame(80, 24, None);
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
fn an_empty_entry_is_not_saved() {
    let fake = Fake::new(rows());
    let mut settings = page(&fake, "sandbox.allow_write");
    press(&mut settings, &fake, &[Key::Enter, Key::Enter]);
    assert!(fake.writes().is_empty());
    assert!(settings.page.as_ref().unwrap().field.is_some());
    assert_eq!(settings.said, ["Type the entry first; nothing was saved."]);
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
|  Rail width    30  project
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
        ("handoff.tokens", Applies::Reload),
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
        applies_text("handoff.tokens", Some(180_000)),
        "changing handoff.tokens needs a reload (~180k tokens)"
    );
    assert_eq!(
        applies_text("handoff.tokens", Some(999)),
        "changing handoff.tokens needs a reload (~999 tokens)"
    );
}

#[test]
fn without_usage_it_says_a_change_needs_a_reload() {
    assert_eq!(
        applies_text("handoff.tokens", None),
        "changing handoff.tokens needs a reload"
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
