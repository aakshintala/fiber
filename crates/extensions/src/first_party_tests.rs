#![allow(
    clippy::expect_used,
    reason = "test fixtures; failures must stop the test"
)]

use super::*;
use std::fs;

#[test]
fn serving_package_resolves_only_first_party_providers() {
    assert_eq!(serving_package("opencode-go"), Some("opencode"));
    assert_eq!(serving_package("openai"), Some("openai"));
    assert_eq!(serving_package("scripted"), None);
}

fn expected() -> Vec<&'static str> {
    PROVIDER_PACKAGES.iter().map(|(name, _)| *name).collect()
}

#[test]
fn empty_home_offers_every_first_party_provider() {
    let home = fakes::TempDir::new("first-party-empty");
    let offered = not_installed_providers(home.path()).expect("test fixture operation succeeds");
    assert_eq!(offered, expected());
    assert!(offered.contains(&"openai"));
    assert!(offered.contains(&"opencode-go"));
    assert!(offered.contains(&"opencode-zen"));
    let mut sorted = offered.clone();
    sorted.sort();
    assert_eq!(offered, sorted);
    assert!(!offered.contains(&"scripted"));
}

fn source(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root.join("source");
    fs::create_dir_all(dir.join("providers")).expect("test fixture operation succeeds");
    fs::write(dir.join("extension.json"), r#"{"name":"fiber.test/custom","version":"0.1.0","fiber":"0.1.0","api":1,"providers":{"openai":["https://example.com"]}}"#).expect("test fixture operation succeeds");
    fs::write(dir.join("providers/openai.json"), r#"{"name":"openai","protocol":"openai-chat","base_url":"https://example.com","models":[]}"#).expect("test fixture operation succeeds");
    dir
}

#[test]
fn installed_declaration_removes_only_its_provider() {
    let root = fakes::TempDir::new("first-party-installed");
    let src = source(root.path());
    let home = root.path().join("home");
    let installed_home = home.clone();
    crate::test_support::bounded(move || {
        crate::plan(
            &installed_home,
            &crate::Request::Path(src),
            "0.1.0",
            &crate::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .expect("test fixture operation succeeds")
        .commit()
        .expect("test fixture operation succeeds");
    });
    let offered = not_installed_providers(&home).expect("test fixture operation succeeds");
    assert_eq!(
        offered,
        expected()
            .into_iter()
            .filter(|name| *name != "openai")
            .collect::<Vec<_>>()
    );
    assert!(!offered.contains(&"scripted"));
}

#[test]
fn damaged_directory_declares_nothing() {
    let root = fakes::TempDir::new("first-party-damaged");
    let src = source(root.path());
    let home = root.path().join("home");
    fs::create_dir_all(home.join("extensions")).expect("test fixture operation succeeds");
    fs::rename(src, home.join("extensions/fiber.test-custom"))
        .expect("test fixture operation succeeds");
    let offered = not_installed_providers(&home).expect("test fixture operation succeeds");
    assert_eq!(offered, expected());
    assert!(!offered.contains(&"scripted"));
}
