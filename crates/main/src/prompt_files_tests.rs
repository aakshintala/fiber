//! Tests for `prompt_files`: `agents_home`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::{agents_home, docs_place};

fn is_installed(place: &r#loop::DocsPlace, docs: &std::path::Path) -> bool {
    matches!(place, r#loop::DocsPlace::Installed(path) if path == docs)
}

fn is_website(place: &r#loop::DocsPlace, url: &str) -> bool {
    matches!(place, r#loop::DocsPlace::Website(got) if *got == url)
}

#[test]
fn docs_place_is_installed_when_docs_is_a_directory() {
    let home = fakes::TempDir::new("fiber-docs-place-dir");
    let docs = home.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    assert!(is_installed(&docs_place(home.path(), "0.4.0"), &docs));
}

#[test]
fn docs_place_is_installed_when_docs_is_a_symlink_to_a_directory() {
    let home = fakes::TempDir::new("fiber-docs-place-link-dir");
    let target = home.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, home.path().join("docs")).unwrap();
    assert!(is_installed(
        &docs_place(home.path(), "0.4.0"),
        &home.path().join("docs")
    ));
}

#[test]
fn docs_place_is_the_stable_website_when_docs_is_missing() {
    let home = fakes::TempDir::new("fiber-docs-place-missing");
    assert!(is_website(
        &docs_place(home.path(), "0.4.0"),
        super::STABLE_DOCS
    ));
}

#[test]
fn docs_place_is_the_nightly_website_for_a_nightly_build() {
    let home = fakes::TempDir::new("fiber-docs-place-nightly");
    assert!(is_website(
        &docs_place(home.path(), "0.4.1-nightly.20261101"),
        super::NIGHTLY_DOCS
    ));
}

#[test]
fn docs_place_is_the_website_when_docs_is_a_regular_file() {
    let home = fakes::TempDir::new("fiber-docs-place-file");
    std::fs::write(home.path().join("docs"), "not a directory").unwrap();
    assert!(is_website(
        &docs_place(home.path(), "0.4.1-nightly.20261101"),
        super::NIGHTLY_DOCS
    ));
}

#[test]
fn docs_place_is_the_website_when_docs_is_a_dangling_symlink() {
    let home = fakes::TempDir::new("fiber-docs-place-dangling");
    std::os::unix::fs::symlink(home.path().join("nowhere"), home.path().join("docs")).unwrap();
    assert!(is_website(
        &docs_place(home.path(), "0.4.0"),
        super::STABLE_DOCS
    ));
}

#[test]
fn docs_place_is_stable_when_the_nightly_date_is_short() {
    let home = fakes::TempDir::new("fiber-docs-place-short-date");
    assert!(is_website(
        &docs_place(home.path(), "0.4.1-nightly.2026110"),
        super::STABLE_DOCS
    ));
}

#[test]
fn the_agents_home_is_an_absolute_home_value() {
    assert_eq!(
        agents_home(Some("/home/alice".into())),
        Some(std::path::PathBuf::from("/home/alice"))
    );
    assert_eq!(agents_home(None), None);
    assert_eq!(agents_home(Some("".into())), None);
    assert_eq!(agents_home(Some("relative/home".into())), None);
}
