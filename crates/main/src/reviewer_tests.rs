//! Choosing step 7's limits from configuration (`docs/configuration.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use crate::settings::block_limits;
use crate::test_support;
use fakes::Deadline;

fn config(overrides: Vec<String>) -> config::Config {
    test_support::config("fiber-reviewer-limits", overrides)
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

/// Where the reviewer connects: no scripted model is reviewed here.
fn here() -> crate::Here {
    crate::Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
    }
}

/// The reviewer's `cache.lifetime` under `overrides`, from
/// [`crate::choose_reviewer`]: the session runs `fake/session`, reviewed by
/// `fake/reviewer`.
fn reviewer_lifetime(overrides: &[&str]) -> contract::events::CacheLifetime {
    let root = fakes::TempDir::new("fiber-reviewer-lifetime");
    let home = root.path().join("home");
    test_support::install_extension(
        &home,
        "extensions/fake",
        serde_json::from_str(
            r#"{"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}"#,
        )
        .unwrap(),
        &[(
            "fake",
            serde_json::from_str(
                r#"{"name": "fake", "models": [
            {"id": "session", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            {"id": "reviewer", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}
        ]}"#,
            )
            .unwrap(),
        )],
    );
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = test_support::load(
        &home,
        &workspace,
        "test",
        ["reviewer.model=fake/reviewer"]
            .iter()
            .chain(overrides)
            .map(|o| (*o).to_owned()),
    );
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    let session = providers.resolve("fake/session").unwrap();
    let here = here();
    let mut lookup = |_: &config::ProviderData| -> Result<
        (String, crate::lua_providers::Access),
        contract::shapes::Failure,
    > {
        Ok((
            "test".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let reviewer = crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None)
        .unwrap()
        .0;
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

/// Two providers sharing one credential name: `alpha` with models
/// `a-session`, `a-want`, `a-pkg`, `a-other`, `org/fast` and `v:2` and a
/// package default of `a-pkg`; `beta` with `b-session` and `b-want` and no
/// package default. Both read a file credential, so `parts_in` starts.
struct Pair {
    root: fakes::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

impl Pair {
    /// The file credential both providers read.
    fn key_file(&self) -> std::path::PathBuf {
        self.root.path().join("key")
    }
}

fn pair(prefix: &str) -> Pair {
    let root = fakes::TempDir::new(prefix);
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let pair = Pair {
        root,
        home,
        workspace,
    };
    std::fs::write(pair.key_file(), "file-key\n").unwrap();
    let key = serde_json::json!({"file": pair.key_file().to_str().unwrap()});
    let model = |id: &str| {
        serde_json::json!({"id": id, "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:9/v1", "context_window": 1000})
    };
    test_support::install_extension(
        &pair.home,
        "extensions/two",
        serde_json::from_str(r#"{"name": "two", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}"#)
            .unwrap(),
        &[
            (
                "alpha",
                serde_json::json!({
                    "name": "alpha",
                    "credential_name": "shared",
                    "credential": key,
                    "reviewer_model": "a-pkg",
                    "models": [model("a-session"), model("a-want"),
                               model("a-pkg"), model("a-other"),
                               model("org/fast"), model("v:2")],
                }),
            ),
            (
                "beta",
                serde_json::json!({
                    "name": "beta",
                    "credential_name": "shared",
                    "credential": key,
                    "models": [model("b-session"), model("b-want")],
                }),
            ),
        ],
    );
    pair
}

fn pair_config(pair: &Pair, overrides: &[&str]) -> config::Config {
    test_support::load(&pair.home, &pair.workspace, "test", overrides)
}

fn pair_providers(pair: &Pair) -> extensions::Providers {
    let (providers, _notices) = extensions::Providers::load(&pair.home).unwrap();
    providers
}

/// `parts_in` under `overrides` on its own thread: startup can wait on
/// provider loading, so a hang fails the test instead of hanging it.
fn bounded_parts(
    pair: &Pair,
    overrides: Vec<String>,
) -> Result<crate::Parts, contract::shapes::Failure> {
    let (home, workspace) = (pair.home.clone(), pair.workspace.clone());
    fakes::within(
        "the startup",
        Deadline::after(std::time::Duration::from_secs(20)),
        move || {
            let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
            let reads = std::sync::Arc::new(crate::switch::Reads::default());
            crate::parts_in(
                home,
                workspace,
                overrides,
                None,
                crate::credential::Labels::default(),
                None,
                clock,
                None,
                None,
                reads,
                log::diag::Process::Session,
                extensions::StateSeed::new(),
                crate::session_extensions::RepositoryScope::default(),
            )
        },
    )
}

/// A failed reviewer: the loop gets it, and `Reviewer` is no `Debug`, so
/// no `expect_err` or `unwrap_err`.
fn reviewer_failed(
    reviewer: Result<
        (r#loop::Reviewer, Option<contract::diag::Observe>),
        contract::shapes::Failure,
    >,
) -> contract::shapes::Failure {
    match reviewer {
        Ok(_) => panic!("the reviewer resolved"),
        Err(failure) => failure,
    }
}

#[test]
fn the_saved_choice_reaches_startup_with_its_debug_factory() {
    let pair = pair("fiber-reviewer-startup");
    // `diagnostics.level` is global-only: only the home file sets debug.
    std::fs::write(
        pair.home.join("config.json"),
        r#"{"diagnostics": {"level": "debug"}}"#,
    )
    .unwrap();
    let parts = bounded_parts(
        &pair,
        [
            "model=alpha/a-session",
            "providers.alpha.reviewer_model=a-want",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    )
    .expect("startup stands");
    let reviewer = parts.reviewer.expect("the reviewer resolved");
    assert_eq!(reviewer.model.reference, "alpha/a-want");
    assert!(
        parts.reviewer_observe.is_some(),
        "the debug reviewer factory reaches startup"
    );
}

#[test]
fn a_missing_reviewer_is_not_a_startup_failure() {
    let pair = pair("fiber-reviewer-startup-none");
    let parts = bounded_parts(
        &pair,
        ["model=beta/b-session"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .expect("startup stands");
    let failure = reviewer_failed(parts.reviewer.map(|reviewer| (reviewer, None)));
    assert_eq!(failure.code, contract::ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
    assert!(parts.reviewer_observe.is_none());
}

/// `routing::reviewer`'s "no source" is `no_model` with the loop's message,
/// whichever way the reviewer is looked up.
#[test]
fn no_source_is_no_model_with_the_loop_message() {
    let pair = pair("fiber-reviewer-none");
    let config = pair_config(&pair, &[]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("beta/b-session").unwrap();
    let failure = match crate::reviewer_model(&providers, &config, &session) {
        Ok(_) => panic!("the reviewer resolved"),
        Err(failure) => failure,
    };
    assert_eq!(failure.code, contract::ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
    let here = here();
    let mut lookup = |_: &config::ProviderData| -> Result<_, contract::shapes::Failure> {
        panic!("a reviewer that is not named reads no credential")
    };
    let failure = reviewer_failed(crate::choose_reviewer(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        None,
    ));
    assert_eq!(failure.code, contract::ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
}
