//! Choosing step 7's limits from configuration (`docs/configuration.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use crate::settings::block_limits;

fn config(overrides: Vec<String>) -> config::Config {
    let root = fakes::TempDir::new("fiber-reviewer-limits");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let project = config::ProjectKey::new("test").unwrap();
    let config = config::Config::load(config::Sources {
        home,
        workspace,
        project,
        overrides,
    })
    .unwrap();
    // `root` is dropped here; the configuration was already read.
    config
}

#[test]
fn block_limits_default_without_configuration() {
    let limits = block_limits(&config(Vec::new()));
    assert_eq!(limits.consecutive, 3);
    assert_eq!(limits.session, 20);
}

#[test]
fn block_limits_reads_configuration() {
    let limits = block_limits(&config(vec![
        "reviewer.block_limits.consecutive=7".into(),
        "reviewer.block_limits.session=9".into(),
    ]));
    assert_eq!(limits.consecutive, 7);
    assert_eq!(limits.session, 9);
}

/// The reviewer's `cache.lifetime` under `overrides`, from
/// [`crate::choose_reviewer`]: the session runs `fake/session`, reviewed by
/// `fake/reviewer`.
fn reviewer_lifetime(overrides: &[&str]) -> contract::events::CacheLifetime {
    let root = fakes::TempDir::new("fiber-reviewer-lifetime");
    let home = root.path().join("home");
    let extension = home.join("extensions").join("fake");
    std::fs::create_dir_all(extension.join("providers")).unwrap();
    std::fs::write(
        extension.join("extension.json"),
        r#"{"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}"#,
    )
    .unwrap();
    std::fs::write(
        extension.join("providers/fake.json"),
        r#"{"name": "fake", "models": [
            {"id": "session", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            {"id": "reviewer", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}
        ]}"#,
    )
    .unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = config::Config::load(config::Sources {
        home: home.clone(),
        workspace,
        project: config::ProjectKey::new("test").unwrap(),
        overrides: ["reviewer.model=fake/reviewer"]
            .iter()
            .chain(overrides)
            .map(|o| (*o).to_owned())
            .collect(),
    })
    .unwrap();
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    let session = providers.resolve("fake/session").unwrap();
    let credential = (Some(contract::Secret::new("key".to_owned())), None);
    let mut lookup = |_: &config::ProviderData| -> Result<
        (String, crate::lua_providers::KeyAndSigner),
        contract::shapes::Failure,
    > { Ok(("default".to_owned(), credential.clone())) };
    let reviewer = crate::choose_reviewer(&providers, &config, &session, &mut lookup).unwrap();
    assert_eq!(reviewer.model.reference, "fake/reviewer");
    reviewer.cache_lifetime
}

/// The reviewer's lifetime is `cache.lifetime` resolved for the
/// reviewer's model (`docs/prompt-cache.md`, "Cache lifetime";
/// `docs/configuration.md`): its per-model key, then the top-level key,
/// then 1 hour.
#[test]
fn the_reviewer_cache_lifetime_resolves_for_the_reviewers_model() {
    use contract::events::CacheLifetime::{FiveMinutes, OneHour};
    assert_eq!(reviewer_lifetime(&[]), OneHour);
    assert_eq!(reviewer_lifetime(&["cache.lifetime=5m"]), FiveMinutes);
    assert_eq!(reviewer_lifetime(&["cache.lifetime=1h"]), OneHour);
    assert_eq!(
        reviewer_lifetime(&[
            "cache.lifetime=1h",
            "models.\"fake/reviewer\".cache.lifetime=5m"
        ]),
        FiveMinutes
    );
    assert_eq!(
        reviewer_lifetime(&[
            "cache.lifetime=5m",
            "models.\"fake/reviewer\".cache.lifetime=1h"
        ]),
        OneHour
    );
    // The session model's own key is not the reviewer's.
    assert_eq!(
        reviewer_lifetime(&["models.\"fake/session\".cache.lifetime=5m"]),
        OneHour
    );
}
