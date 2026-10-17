//! Tests for the theme setting: which names read a file, and which file.
//! The lookup itself lives in `config`; these pin `main`'s wiring to it.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use config::Config;

use super::{named, setting};

/// The configuration for `home`, with `tui.theme` set to `theme` when given.
fn config(home: &Path, theme: Option<&str>) -> Config {
    crate::test_support::load(
        home,
        home,
        "-w",
        theme.map(|theme| format!("tui.theme={theme}")),
    )
}

/// The setting for `theme`, and every path the reader was asked for. The
/// reader answers `answer`.
fn read_with(
    theme: Option<&str>,
    answer: &dyn Fn() -> io::Result<String>,
) -> (tui::ThemeSetting, Vec<PathBuf>) {
    let dir = fakes::TempDir::new("fiber-theme-setting");
    let asked = RefCell::new(Vec::new());
    let read = |path: &Path| {
        asked.borrow_mut().push(path.to_path_buf());
        answer()
    };
    let got = setting(Path::new("/home"), &config(dir.path(), theme), &read);
    (got, asked.into_inner())
}

/// A reader that must not be called.
fn never() -> io::Result<String> {
    Err(io::Error::other("read"))
}

#[test]
fn unset_and_auto_follow() {
    for theme in [None, Some("auto")] {
        let (got, asked) = read_with(theme, &never);
        assert!(
            matches!(got, tui::ThemeSetting::Follow),
            "{theme:?}: {got:?}"
        );
        assert!(asked.is_empty(), "{asked:?}");
    }
}

#[test]
fn dark_and_light_are_built_ins_and_read_nothing() {
    let (dark, asked) = read_with(Some("dark"), &never);
    assert!(matches!(dark, tui::ThemeSetting::Dark), "{dark:?}");
    assert!(asked.is_empty(), "{asked:?}");
    let (light, asked) = read_with(Some("light"), &never);
    assert!(matches!(light, tui::ThemeSetting::Light), "{light:?}");
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn a_name_reads_themes_name_json_under_home() {
    let (got, asked) = read_with(Some("solar"), &|| Ok("{}".to_owned()));
    assert_eq!(asked, [PathBuf::from("/home/themes/solar.json")]);
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "solar");
    assert_eq!(text, Ok("{}".to_owned()));
}

#[test]
fn a_read_error_is_carried_as_text() {
    let (got, _) = read_with(Some("solar"), &|| Err(io::Error::other("gone")));
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "solar");
    assert_eq!(text, Err("gone".to_owned()));
}

/// A healthy install record, as `extensions/<dir>/.fiber.json` holds it.
const RECORD: &str = r#"{"name":"x","version":"1.0.0","requested":true,"source":{"path":"/p"}}"#;

/// Makes `dir` a healthy extension in `home` named `name`.
fn healthy_named(home: &Path, dir: &str, name: &str) {
    let path = home.join("extensions").join(dir);
    std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let record = format!(
        r#"{{"name":"{name}","version":"1.0.0","requested":true,"source":{{"path":"/p"}}}}"#
    );
    std::fs::write(path.join(".fiber.json"), record).unwrap_or_else(|e| panic!("write: {e}"));
}

/// Makes `dir` a healthy extension in `home`.
fn healthy(home: &Path, dir: &str) {
    healthy_named(home, dir, "x");
}

/// The setting for `name` in `home`, and every path the reader was asked
/// for. The reader answers `answer` per path.
fn named_with(
    home: &Path,
    name: &str,
    answer: &dyn Fn(&Path) -> io::Result<String>,
) -> (tui::ThemeSetting, Vec<PathBuf>) {
    let asked = RefCell::new(Vec::new());
    let read = |path: &Path| {
        asked.borrow_mut().push(path.to_path_buf());
        answer(path)
    };
    let got = named(home, Some(name), &read, &|_: &str| true);
    (got, asked.into_inner())
}

/// A reader that answers `Ok` for the paths in `found` and `NotFound`
/// otherwise.
fn found(
    found: &std::collections::HashMap<PathBuf, String>,
) -> impl Fn(&Path) -> io::Result<String> + '_ {
    |path| {
        found
            .get(path)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::NotFound, "gone")))
    }
}

#[test]
fn damaged_and_in_progress_packages_are_never_asked() {
    let home = fakes::TempDir::new("fiber-theme-skipped");
    healthy(home.path(), "acme");
    let broken = home.path().join("extensions").join("broken");
    std::fs::create_dir_all(broken.join("themes")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(broken.join("themes").join("dusk.json"), "broken")
        .unwrap_or_else(|e| panic!("write: {e}"));
    let staging = home.path().join("extensions").join(".staging");
    std::fs::create_dir_all(&staging).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(staging.join(".fiber.json"), RECORD).unwrap_or_else(|e| panic!("write: {e}"));
    std::fs::create_dir_all(staging.join("themes")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(staging.join("themes").join("dusk.json"), "staging")
        .unwrap_or_else(|e| panic!("write: {e}"));
    let missing = |_: &Path| Err(io::Error::new(io::ErrorKind::NotFound, "gone"));
    let (_, asked) = named_with(home.path(), "dusk", &missing);
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            home.path()
                .join("extensions")
                .join("acme")
                .join("themes")
                .join("dusk.json"),
        ]
    );
}

/// The setting for `name` in `home`, asking only enabled extensions.
/// The reader answers `answer` per path.
fn named_enabled(
    home: &Path,
    name: &str,
    answer: &dyn Fn(&Path) -> io::Result<String>,
    enabled: &dyn Fn(&str) -> bool,
) -> (tui::ThemeSetting, Vec<PathBuf>) {
    let asked = RefCell::new(Vec::new());
    let read = |path: &Path| {
        asked.borrow_mut().push(path.to_path_buf());
        answer(path)
    };
    let got = named(home, Some(name), &read, enabled);
    (got, asked.into_inner())
}

#[test]
fn a_disabled_extensions_file_reads_as_absent() {
    let home = fakes::TempDir::new("fiber-theme-disabled-absent");
    healthy_named(home.path(), "acme", "acme");
    let acme_file = home
        .path()
        .join("extensions")
        .join("acme")
        .join("themes")
        .join("dusk.json");
    let answers = [(acme_file.clone(), "acme".to_owned())]
        .into_iter()
        .collect();
    let (got, asked) = named_enabled(home.path(), "dusk", &found(&answers), &|_: &str| false);
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "dusk");
    // The disabled file is skipped, so the text is home's absence error:
    // one notice, then the terminal follows its appearance.
    assert_eq!(text, Err("gone".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn setting_with_a_disabled_extension_reads_it_as_absent() {
    let home = fakes::TempDir::new("fiber-theme-setting-disabled");
    healthy_named(home.path(), "acme", "acme");
    std::fs::create_dir_all(home.path().join("extensions").join("acme").join("themes"))
        .unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(
        home.path()
            .join("extensions")
            .join("acme")
            .join("themes")
            .join("dusk.json"),
        "acme",
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    std::fs::write(
        home.path().join("config.json"),
        r#"{"extensions": {"acme": {"enabled": false}}, "tui": {"theme": "dusk"}}"#,
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    let config = crate::test_support::load(home.path(), home.path(), "-w", Vec::<String>::new());
    let got = setting(home.path(), &config, &|path| std::fs::read_to_string(path));
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "dusk");
    assert!(text.is_err(), "{text:?}");
}
