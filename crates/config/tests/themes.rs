//! Tests for `config`'s theme lookup: source order, error rules, the
//! shared name rule, and listing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use config::{theme_names, theme_text};

/// The lookup of `name` in `home` with `packages`, and every path the
/// reader was asked for. The reader answers `answer` per path.
fn text_with(
    home: &Path,
    name: &str,
    packages: &[(String, PathBuf)],
    enabled: &dyn Fn(&str) -> bool,
    answer: &dyn Fn(&Path) -> io::Result<String>,
) -> (Result<String, String>, Vec<PathBuf>) {
    let asked = RefCell::new(Vec::new());
    let read = |path: &Path| {
        asked.borrow_mut().push(path.to_path_buf());
        answer(path)
    };
    let got = theme_text(home, name, packages, enabled, &read);
    (got, asked.into_inner())
}

/// A reader that answers `Ok` for the paths in `found` and `NotFound`
/// otherwise.
fn found(found: &HashMap<PathBuf, String>) -> impl Fn(&Path) -> io::Result<String> + '_ {
    |path| {
        found
            .get(path)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::NotFound, "gone")))
    }
}

fn always_enabled(_: &str) -> bool {
    true
}

fn home_dir(home: &fakes::TempDir) -> PathBuf {
    home.path().to_path_buf()
}

fn package(home: &fakes::TempDir, dir: &str) -> (String, PathBuf) {
    (dir.to_owned(), home.path().join("extensions").join(dir))
}

#[test]
fn a_home_file_wins_without_asking_any_package() {
    let home = fakes::TempDir::new("fiber-config-theme-home-wins");
    let packages = [package(&home, "acme")];
    let answers = [(
        home_dir(&home).join("themes").join("dusk.json"),
        "home".to_owned(),
    )]
    .into_iter()
    .collect();
    let (got, asked) = text_with(
        home.path(),
        "dusk",
        &packages,
        &always_enabled,
        &found(&answers),
    );
    assert_eq!(got, Ok("home".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_home_file_wins_without_calling_enabled() {
    let home = fakes::TempDir::new("fiber-config-theme-no-enabled");
    let packages = [package(&home, "acme")];
    let answers = [(
        home.path().join("themes").join("dusk.json"),
        "home".to_owned(),
    )]
    .into_iter()
    .collect();
    let enabled = |_: &str| -> bool { panic!("enabled must not be called") };
    let (got, _) = text_with(home.path(), "dusk", &packages, &enabled, &found(&answers));
    assert_eq!(got, Ok("home".to_owned()));
}

#[test]
fn a_missing_home_file_falls_through_to_the_package() {
    let home = fakes::TempDir::new("fiber-config-theme-package");
    let acme = home.path().join("extensions").join("acme");
    let packages = [(String::from("acme"), acme.clone())];
    let answers = [(acme.join("themes").join("dusk.json"), "acme".to_owned())]
        .into_iter()
        .collect();
    let (got, asked) = text_with(
        home.path(),
        "dusk",
        &packages,
        &always_enabled,
        &found(&answers),
    );
    assert_eq!(got, Ok("acme".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            acme.join("themes").join("dusk.json"),
        ]
    );
}

#[test]
fn a_present_but_invalid_home_file_does_not_fall_through() {
    let home = fakes::TempDir::new("fiber-config-theme-invalid");
    let acme = home.path().join("extensions").join("acme");
    let packages = [(String::from("acme"), acme.clone())];
    let answers = [
        (
            home.path().join("themes").join("dusk.json"),
            "not json".to_owned(),
        ),
        (acme.join("themes").join("dusk.json"), "acme".to_owned()),
    ]
    .into_iter()
    .collect();
    let (got, asked) = text_with(
        home.path(),
        "dusk",
        &packages,
        &always_enabled,
        &found(&answers),
    );
    assert_eq!(got, Ok("not json".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_home_read_error_other_than_absence_is_the_theme() {
    let home = fakes::TempDir::new("fiber-config-theme-denied");
    let packages = [package(&home, "acme")];
    let denied = |path: &Path| {
        if path == home.path().join("themes").join("dusk.json") {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        } else {
            panic!("asked {path:?}");
        }
    };
    let (got, asked) = text_with(home.path(), "dusk", &packages, &always_enabled, &denied);
    assert_eq!(got, Err("denied".to_owned()));
    assert_eq!(asked, [home.path().join("themes").join("dusk.json")]);
}

#[test]
fn a_package_read_error_other_than_absence_is_the_theme() {
    let home = fakes::TempDir::new("fiber-config-theme-package-denied");
    let a = home.path().join("extensions").join("a");
    let b = home.path().join("extensions").join("b");
    let packages = [(String::from("a"), a.clone()), (String::from("b"), b)];
    let a_file = a.join("themes").join("dusk.json");
    let denied = |path: &Path| {
        if path == a_file {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        } else if path.starts_with(home.path().join("extensions").join("b")) {
            panic!("asked {path:?}");
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        }
    };
    let (got, asked) = text_with(home.path(), "dusk", &packages, &always_enabled, &denied);
    assert_eq!(got, Err("denied".to_owned()));
    assert_eq!(asked.last(), Some(&a_file));
}

#[test]
fn the_first_package_in_the_given_order_wins() {
    let home = fakes::TempDir::new("fiber-config-theme-order");
    let a = home.path().join("extensions").join("a");
    let b = home.path().join("extensions").join("b");
    // The given order decides, not the directory order.
    let packages = [
        (String::from("b"), b.clone()),
        (String::from("a"), a.clone()),
    ];
    let answers = [
        (a.join("themes").join("dusk.json"), "a".to_owned()),
        (b.join("themes").join("dusk.json"), "b".to_owned()),
    ]
    .into_iter()
    .collect();
    let (got, asked) = text_with(
        home.path(),
        "dusk",
        &packages,
        &always_enabled,
        &found(&answers),
    );
    assert_eq!(got, Ok("b".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            b.join("themes").join("dusk.json"),
        ]
    );
}

#[test]
fn when_every_source_lacks_the_file_the_text_is_homes_error() {
    let home = fakes::TempDir::new("fiber-config-theme-absent");
    let a = home.path().join("extensions").join("a");
    let b = home.path().join("extensions").join("b");
    let packages = [
        (String::from("a"), a.clone()),
        (String::from("b"), b.clone()),
    ];
    let home_file = home.path().join("themes").join("dusk.json");
    let missing = |path: &Path| -> io::Result<String> {
        if path == home_file {
            Err(io::Error::new(io::ErrorKind::NotFound, "home gone"))
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        }
    };
    let (got, asked) = text_with(home.path(), "dusk", &packages, &always_enabled, &missing);
    assert_eq!(got, Err("home gone".to_owned()));
    assert_eq!(
        asked,
        [
            home_file,
            a.join("themes").join("dusk.json"),
            b.join("themes").join("dusk.json"),
        ]
    );
}

#[test]
fn bad_names_are_refused_without_a_read() {
    let home = fakes::TempDir::new("fiber-config-theme-bad-names");
    let packages = [package(&home, "acme")];
    for name in [
        "", ".hidden", "..", "a/b", "a\\b", "/etc/x", "auto", "dark", "light",
    ] {
        let asked = RefCell::new(Vec::new());
        let read = |path: &Path| -> io::Result<String> {
            asked.borrow_mut().push(path.to_path_buf());
            panic!("a refused name reads nothing");
        };
        let got = theme_text(home.path(), name, &packages, &always_enabled, &read);
        assert_eq!(got, Err("not a theme name".to_owned()), "{name}");
        assert!(asked.borrow().is_empty(), "{name}: {asked:?}");
    }
    // A dot past the first character is part of an ordinary name.
    let asked = RefCell::new(Vec::new());
    let read = |path: &Path| -> io::Result<String> {
        asked.borrow_mut().push(path.to_path_buf());
        Ok(String::new())
    };
    let got = theme_text(home.path(), "a.b", &packages, &always_enabled, &read);
    assert_eq!(got, Ok(String::new()));
    assert_eq!(
        asked.into_inner(),
        [home.path().join("themes").join("a.b.json")]
    );
}

/// Writes `text` to the theme file `name` in `dir`, making it.
fn write_theme(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(dir.join(name), "{}").unwrap_or_else(|e| panic!("write: {e}"));
}

#[test]
fn names_lists_home_and_package_themes_once_sorted() {
    let home = fakes::TempDir::new("fiber-config-theme-names");
    let acme = home.path().join("extensions").join("acme");
    let packages = [(String::from("acme"), acme.clone())];
    write_theme(&home.path().join("themes"), "solar.json");
    write_theme(&home.path().join("themes"), "dusk.json");
    write_theme(
        &home.path().join("extensions").join("acme").join("themes"),
        "dusk.json",
    );
    write_theme(&acme.join("themes"), "tide.json");
    assert_eq!(
        theme_names(home.path(), &packages, &always_enabled),
        ["dusk", "solar", "tide"]
    );
}

#[test]
fn names_leaves_out_what_loading_refuses() {
    let home = fakes::TempDir::new("fiber-config-theme-names-left-out");
    let acme = home.path().join("extensions").join("acme");
    let acme_themes = acme.join("themes");
    let packages = [(String::from("acme"), acme.clone())];
    let home_themes = home.path().join("themes");
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
    assert_eq!(
        theme_names(home.path(), &packages, &always_enabled),
        ["a.b", "tide"]
    );
}

#[test]
fn names_leaves_out_a_disabled_extensions_themes() {
    let home = fakes::TempDir::new("fiber-config-theme-names-disabled");
    let acme = home.path().join("extensions").join("acme");
    let other = home.path().join("extensions").join("other");
    let packages = [(String::from("acme"), acme), (String::from("other"), other)];
    write_theme(
        &home.path().join("extensions").join("acme").join("themes"),
        "dusk.json",
    );
    write_theme(
        &home.path().join("extensions").join("other").join("themes"),
        "tide.json",
    );
    assert_eq!(
        theme_names(home.path(), &packages, &|name| name != "acme"),
        ["tide"]
    );
}

#[test]
fn a_disabled_extensions_same_named_file_is_skipped_for_a_later_enabled_one() {
    let home = fakes::TempDir::new("fiber-config-theme-disabled-order");
    let aaa = home.path().join("extensions").join("aaa");
    let zzz = home.path().join("extensions").join("zzz");
    let packages = [
        (String::from("off"), aaa.clone()),
        (String::from("on"), zzz.clone()),
    ];
    let answers = [
        (aaa.join("themes").join("dusk.json"), "off".to_owned()),
        (zzz.join("themes").join("dusk.json"), "on".to_owned()),
    ]
    .into_iter()
    .collect();
    let enabled = |name: &str| name != "off";
    let (got, asked) = text_with(home.path(), "dusk", &packages, &enabled, &found(&answers));
    assert_eq!(got, Ok("on".to_owned()));
    assert_eq!(
        asked,
        [
            home.path().join("themes").join("dusk.json"),
            zzz.join("themes").join("dusk.json"),
        ]
    );
}

#[test]
fn a_linked_package_theme_file_loads() {
    let home = fakes::TempDir::new("fiber-config-theme-link");
    let acme_themes = home.path().join("extensions").join("acme").join("themes");
    std::fs::create_dir_all(&acme_themes).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let target = home.path().join("elsewhere.json");
    std::fs::write(&target, "linked").unwrap_or_else(|e| panic!("write: {e}"));
    std::os::unix::fs::symlink(&target, acme_themes.join("dusk.json"))
        .unwrap_or_else(|e| panic!("link: {e}"));
    let acme = home.path().join("extensions").join("acme");
    let packages = [(String::from("acme"), acme)];
    let got = theme_text(home.path(), "dusk", &packages, &always_enabled, &|path| {
        std::fs::read_to_string(path)
    });
    assert_eq!(got, Ok("linked".to_owned()));
}
