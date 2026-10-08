//! `docs/state.md`, "Override": `FIBER_HOME` must be absolute, is never
//! silently replaced by `~/.fiber`, and a missing directory is created 0700.

mod common;

use std::ffi::OsString;
use std::fs;

use common::{Setup, mode};
use config::{create_fiber_home, fiber_home, fiber_home_path};
use contract::ErrorCode;

#[test]
fn fiber_home_names_the_directory_and_creates_it_0700() {
    let setup = Setup::new();
    let wanted = setup.root().join("elsewhere/fiber");
    let home = fiber_home(Some(wanted.clone().into()), Some("/nonexistent".into())).unwrap();
    assert_eq!(home, wanted);
    assert_eq!(mode(&home), 0o700);
}

#[test]
fn without_fiber_home_it_is_dot_fiber_in_the_home_directory() {
    let setup = Setup::new();
    let home = fiber_home(None, Some(setup.root().join("alice").into())).unwrap();
    assert_eq!(home, setup.root().join("alice/.fiber"));
    assert_eq!(mode(&home), 0o700);
}

#[test]
fn an_existing_fiber_home_is_used_as_it_is() {
    let setup = Setup::new();
    fs::write(setup.home().join("config.json"), "{}").unwrap();
    let home = fiber_home(Some(setup.home().into()), None).unwrap();
    assert!(home.join("config.json").exists());
}

#[test]
fn an_empty_or_relative_fiber_home_is_an_error_naming_it() {
    for value in ["", "relative/fiber", "."] {
        let e = fiber_home(Some(OsString::from(value)), Some("/Users/alice".into())).unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage, "{value:?}");
        assert!(e.to_string().starts_with("FIBER_HOME "), "{e}");
    }
}

#[test]
fn with_no_usable_home_directory_it_asks_for_fiber_home() {
    for home in [
        None,
        Some(OsString::from("relative")),
        Some(OsString::new()),
    ] {
        let e = fiber_home(None, home).unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage);
        assert_eq!(
            e.to_string(),
            "HOME is not an absolute path, so Fiber home is unknown; set FIBER_HOME."
        );
    }
}

#[test]
fn a_fiber_home_that_cannot_be_created_is_an_error() {
    let setup = Setup::new();
    let file = setup.root().join("file");
    fs::write(&file, "").unwrap();
    let e = fiber_home(Some(file.join("home").into()), None).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
}

#[test]
fn fiber_home_path_names_the_directory_without_creating_it() {
    let setup = Setup::new();
    let wanted = setup.root().join("elsewhere/fiber");
    let home = fiber_home_path(Some(wanted.clone().into()), None).unwrap();
    assert_eq!(home, wanted);
    assert!(!wanted.exists());
    assert!(!setup.root().join("elsewhere").exists());
    let dot = fiber_home_path(None, Some(setup.root().join("bob").into())).unwrap();
    assert_eq!(dot, setup.root().join("bob/.fiber"));
    assert!(!setup.root().join("bob").exists());
}

#[test]
fn fiber_home_path_gives_the_same_refusals() {
    for value in ["", "relative/fiber"] {
        let path = fiber_home_path(Some(OsString::from(value)), None).unwrap_err();
        let made = fiber_home(Some(OsString::from(value)), None).unwrap_err();
        assert_eq!(path.to_string(), made.to_string(), "{value:?}");
        assert_eq!(path.code(), ErrorCode::Usage);
    }
    let path = fiber_home_path(None, Some("relative".into())).unwrap_err();
    assert_eq!(
        path.to_string(),
        "HOME is not an absolute path, so Fiber home is unknown; set FIBER_HOME."
    );
}

#[test]
fn create_fiber_home_makes_it_and_its_parents_0700() {
    let setup = Setup::new();
    let home = setup.root().join("a/b/fiber");
    create_fiber_home(&home).unwrap();
    assert_eq!(mode(&home), 0o700);
    create_fiber_home(&home).unwrap();
    let file = setup.root().join("file");
    fs::write(&file, "").unwrap();
    let e = create_fiber_home(&file.join("home")).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
}
