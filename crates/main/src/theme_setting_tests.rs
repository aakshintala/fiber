//! Tests for the theme setting: which names read a file, and which file.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use config::{Config, Sources};

use super::setting;

/// The configuration for `home`, with `tui.theme` set to `theme` when given.
fn config(home: &Path, theme: Option<&str>) -> Config {
    let project = config::ProjectKey::new("-w").unwrap_or_else(|err| panic!("key: {err}"));
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project,
        overrides: theme
            .map(|theme| format!("tui.theme={theme}"))
            .into_iter()
            .collect(),
    })
    .unwrap_or_else(|err| panic!("config: {err}"))
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

#[test]
fn bad_names_are_refused_without_a_read() {
    for name in ["", ".hidden", "..", "a/b", "a\\b", "/etc/x"] {
        let (got, asked) = read_with(Some(name), &never);
        assert!(asked.is_empty(), "{name}: {asked:?}");
        let tui::ThemeSetting::File { name: got, text } = got else {
            panic!("{name}: not a theme file");
        };
        assert_eq!(got, name);
        assert_eq!(text, Err("not a theme name".to_owned()));
    }
    // A dot past the first character is part of an ordinary name.
    let (_, asked) = read_with(Some("a.b"), &|| Ok(String::new()));
    assert_eq!(asked, [PathBuf::from("/home/themes/a.b.json")]);
}
