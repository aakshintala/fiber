//! Tests for the theme setting: which names read a file, and which file.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use config::{Config, Sources};

use super::{named, names, setting};

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

/// Writes `text` to the theme file `name` in `dir`, making it.
fn write_theme(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(dir.join(name), "{}").unwrap_or_else(|e| panic!("write: {e}"));
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
fn a_missing_home_file_falls_through_to_the_package() {
    let home = fakes::TempDir::new("fiber-theme-package");
    healthy(home.path(), "acme");
    let acme = home.path().join("extensions").join("acme");
    std::fs::create_dir_all(acme.join("themes")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(acme.join("themes").join("dusk.json"), "acme")
        .unwrap_or_else(|e| panic!("write: {e}"));
    let answers = [(acme.join("themes").join("dusk.json"), "acme".to_owned())]
        .into_iter()
        .collect();
    let (got, asked) = named_with(home.path(), "dusk", &found(&answers));
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!((name.as_str(), text), ("dusk", Ok("acme".to_owned())));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            acme.join("themes").join("dusk.json"),
        ]
    );
}

#[test]
fn a_home_file_wins_without_asking_any_package() {
    let home = fakes::TempDir::new("fiber-theme-home-wins");
    healthy(home.path(), "acme");
    let answers = [(
        home.path().join("themes").join("dusk.json"),
        "home".to_owned(),
    )]
    .into_iter()
    .collect();
    let (got, asked) = named_with(home.path(), "dusk", &found(&answers));
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Ok("home".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_present_but_invalid_home_file_does_not_fall_through() {
    let home = fakes::TempDir::new("fiber-theme-invalid");
    healthy(home.path(), "acme");
    let answers = [
        (
            home.path().join("themes").join("dusk.json"),
            "not json".to_owned(),
        ),
        (
            home.path()
                .join("extensions")
                .join("acme")
                .join("themes")
                .join("dusk.json"),
            "acme".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    let (got, asked) = named_with(home.path(), "dusk", &found(&answers));
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Ok("not json".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_home_read_error_other_than_absence_is_the_theme() {
    let home = fakes::TempDir::new("fiber-theme-denied");
    healthy(home.path(), "acme");
    let denied = |path: &Path| {
        if path == home.path().join("themes").join("dusk.json") {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        } else {
            panic!("asked {path:?}");
        }
    };
    let (got, asked) = named_with(home.path(), "dusk", &denied);
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Err("denied".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_package_read_error_other_than_absence_is_the_theme() {
    let home = fakes::TempDir::new("fiber-theme-package-denied");
    healthy(home.path(), "a");
    healthy(home.path(), "b");
    let a_file = home
        .path()
        .join("extensions")
        .join("a")
        .join("themes")
        .join("dusk.json");
    let denied = |path: &Path| {
        if path == a_file {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        } else if path.starts_with(home.path().join("extensions").join("b")) {
            panic!("asked {path:?}");
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        }
    };
    let (got, asked) = named_with(home.path(), "dusk", &denied);
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Err("denied".to_owned()));
    assert_eq!(asked.last(), Some(&a_file));
}

#[test]
fn the_first_package_by_directory_name_wins() {
    let home = fakes::TempDir::new("fiber-theme-order");
    healthy(home.path(), "b");
    healthy(home.path(), "a");
    let answers = [
        (
            home.path()
                .join("extensions")
                .join("a")
                .join("themes")
                .join("dusk.json"),
            "a".to_owned(),
        ),
        (
            home.path()
                .join("extensions")
                .join("b")
                .join("themes")
                .join("dusk.json"),
            "b".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    let (got, asked) = named_with(home.path(), "dusk", &found(&answers));
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Ok("a".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            home.path()
                .join("extensions")
                .join("a")
                .join("themes")
                .join("dusk.json"),
        ]
    );
}

#[test]
fn when_every_source_lacks_the_file_the_text_is_homes_error() {
    let home = fakes::TempDir::new("fiber-theme-absent");
    healthy(home.path(), "b");
    healthy(home.path(), "a");
    let missing = |_: &Path| Err(io::Error::new(io::ErrorKind::NotFound, "gone"));
    let (got, asked) = named_with(home.path(), "dusk", &missing);
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Err("gone".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            home.path()
                .join("extensions")
                .join("a")
                .join("themes")
                .join("dusk.json"),
            home.path()
                .join("extensions")
                .join("b")
                .join("themes")
                .join("dusk.json"),
        ]
    );
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

#[test]
fn names_lists_home_and_package_themes_once_sorted() {
    let home = fakes::TempDir::new("fiber-theme-names");
    healthy(home.path(), "acme");
    write_theme(&home.path().join("themes"), "solar.json");
    write_theme(&home.path().join("themes"), "dusk.json");
    let acme = home.path().join("extensions").join("acme").join("themes");
    write_theme(&acme, "dusk.json");
    write_theme(&acme, "tide.json");
    assert_eq!(
        names(home.path(), &|_: &str| true),
        ["dusk", "solar", "tide"]
    );
}

#[test]
fn names_leaves_out_what_loading_refuses() {
    let home = fakes::TempDir::new("fiber-theme-names-left-out");
    healthy(home.path(), "acme");
    let home_themes = home.path().join("themes");
    let acme_themes = home.path().join("extensions").join("acme").join("themes");
    for dir in [&home_themes, &acme_themes] {
        for name in [
            ".json",
            ".hidden.json",
            "a\\b.json",
            "auto.json",
            "dark.json",
            "light.json",
            "notes.txt",
        ] {
            write_theme(dir, name);
        }
        std::fs::create_dir_all(dir.join("dir.json")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    write_theme(&home_themes, "a.b.json");
    write_theme(&acme_themes, "tide.json");
    assert_eq!(names(home.path(), &|_: &str| true), ["a.b", "tide"]);
}

#[test]
fn a_backslash_name_is_refused_without_a_read() {
    let home = fakes::TempDir::new("fiber-theme-backslash");
    let (got, asked) = named_with(home.path(), "a\\b", &|_| {
        panic!("a refused name reads nothing");
    });
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "a\\b");
    assert_eq!(text, Err("not a theme name".to_owned()));
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn a_linked_package_theme_file_loads() {
    let home = fakes::TempDir::new("fiber-theme-link");
    healthy(home.path(), "acme");
    let acme_themes = home.path().join("extensions").join("acme").join("themes");
    std::fs::create_dir_all(&acme_themes).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let target = home.path().join("elsewhere.json");
    std::fs::write(&target, "linked").unwrap_or_else(|e| panic!("write: {e}"));
    std::os::unix::fs::symlink(&target, acme_themes.join("dusk.json"))
        .unwrap_or_else(|e| panic!("link: {e}"));
    let got = named(
        home.path(),
        Some("dusk"),
        &|path| std::fs::read_to_string(path),
        &|_: &str| true,
    );
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!((name.as_str(), text), ("dusk", Ok("linked".to_owned())));
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
fn an_explicitly_enabled_extension_still_supplies_its_file() {
    let home = fakes::TempDir::new("fiber-theme-enabled-true");
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
    let (got, _) = named_enabled(home.path(), "dusk", &found(&answers), &|name| {
        assert_eq!(name, "acme");
        true
    });
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Ok("acme".to_owned()));
}

#[test]
fn a_disabled_extensions_same_named_file_is_skipped_for_a_later_enabled_one() {
    let home = fakes::TempDir::new("fiber-theme-disabled-order");
    healthy_named(home.path(), "aaa", "off");
    healthy_named(home.path(), "zzz", "on");
    let answers = [
        (
            home.path()
                .join("extensions")
                .join("aaa")
                .join("themes")
                .join("dusk.json"),
            "off".to_owned(),
        ),
        (
            home.path()
                .join("extensions")
                .join("zzz")
                .join("themes")
                .join("dusk.json"),
            "on".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    let enabled = |name: &str| name != "off";
    let (got, asked) = named_enabled(home.path(), "dusk", &found(&answers), &enabled);
    let tui::ThemeSetting::File { text, .. } = got else {
        panic!("not a theme file");
    };
    assert_eq!(text, Ok("on".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            home.path()
                .join("extensions")
                .join("zzz")
                .join("themes")
                .join("dusk.json"),
        ]
    );
}

#[test]
fn names_leaves_out_a_disabled_extensions_themes() {
    let home = fakes::TempDir::new("fiber-theme-names-disabled");
    healthy_named(home.path(), "acme", "acme");
    healthy_named(home.path(), "other", "other");
    write_theme(
        &home.path().join("extensions").join("acme").join("themes"),
        "dusk.json",
    );
    write_theme(
        &home.path().join("extensions").join("other").join("themes"),
        "tide.json",
    );
    assert_eq!(names(home.path(), &|name| name != "acme"), ["tide"]);
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
    let project = config::ProjectKey::new("-w").unwrap_or_else(|err| panic!("key: {err}"));
    let config = Config::load(Sources {
        home: home.path().to_path_buf(),
        workspace: home.path().to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .unwrap_or_else(|err| panic!("config: {err}"));
    let got = setting(home.path(), &config, &|path| std::fs::read_to_string(path));
    let tui::ThemeSetting::File { name, text } = got else {
        panic!("not a theme file");
    };
    assert_eq!(name, "dusk");
    assert!(text.is_err(), "{text:?}");
}
