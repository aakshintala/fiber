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
use fakes::MUST_SUCCEED_WITHIN;

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

/// The reviewer's provider carries the lookup of the Lua provider its
/// `Access` names, and none without one (`docs/model-routing.md`, "Cost").
#[test]
fn the_reviewer_connects_with_the_lua_provider_its_access_names() {
    let root = fakes::TempDir::new("fiber-reviewer-lua-cost");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let package =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/openrouter");
    extensions::plan(
        &home,
        &extensions::Request::Path(package),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    // The data file holds no models: the cached entry stands in for a
    // discovery run, so resolving the reference never touches the network.
    config::write_model_cache(
        &home,
        "openrouter",
        &serde_json::json!([{
            "id": "z-ai/glm-5.3-flash",
            "protocol": "openai-completions",
            "base_url": "https://openrouter.ai/api/v1",
            "compat": {"cache_key_field": "session_id", "reasoning_object": true},
            "context_window": 1048576,
            "max_output_tokens": 943717,
            "input": ["text", "image", "video"],
            "cost": {"input": 0.15, "output": 0.5, "cache_read": 0.03},
        }]),
    )
    .unwrap();
    let reference = "openrouter/z-ai/glm-5.3-flash";
    let config = test_support::load(
        &home,
        &workspace,
        "test",
        [format!("reviewer.model={reference}")],
    );
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    let session_extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        std::sync::Arc::new(tools::PathLocks::new()),
        None,
    );
    let lua = session_extensions
        .lua_providers()
        .iter()
        .find(|(_, provider)| provider.name() == "openrouter")
        .map(|(_, provider)| std::sync::Arc::clone(provider))
        .expect("the package registers its provider");
    let session = providers.resolve(reference).unwrap();
    let here = here();
    for (with, carries) in [(Some(&lua), true), (None, false)] {
        let mut lookup = |_: &config::ProviderData| {
            Ok((
                "test".to_owned(),
                crate::lua_providers::Access::new(
                    with,
                    (Some(contract::Secret::new("key".to_owned())), None),
                ),
            ))
        };
        let reviewer =
            crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None)
                .unwrap()
                .0;
        assert_eq!(reviewer.model.reference, reference);
        assert_eq!(reviewer.provider.cost_lookup().is_some(), carries);
    }
}

/// A model call through the reviewer `choose_reviewer` builds fetches quota
/// for the label its lookup returned, which proves `choose_reviewer` hands
/// that label to `connect` (`docs/tools.md`, "Provider quota").
#[test]
fn the_reviewer_fetches_quota_for_the_lookup_label() {
    let root = fakes::TempDir::new("fiber-reviewer-quota");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, "{\"windows\": []}")]).unwrap();
    test_support::install_extension(
        &home,
        "extensions/qp",
        serde_json::from_str(
            r#"{"name": "qp", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}"#,
        )
        .unwrap(),
        &[(
            "qp",
            serde_json::from_str(
                r#"{"name": "qp", "models": [
            {"id": "m1", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000},
            {"id": "m2", "protocol": "openai-responses", "base_url": "http://127.0.0.1:9/v1", "context_window": 1000}
        ]}"#,
            )
            .unwrap(),
        )],
    );
    std::fs::write(
        home.join("extensions/qp/providers/qp.lua"),
        format!(
            "fiber.provider(\"qp\", {{ quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{}/\" .. a.label }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }} }})",
            server.url(),
        ),
    )
    .unwrap();
    let config = test_support::load(&home, &workspace, "test", ["reviewer.model=qp/m2"]);
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    // `SessionExtensions::load` can wait on Lua registration, so it runs on
    // its own thread under a deadline instead of hanging the test.
    let (ext_done, ext_ready) = std::sync::mpsc::channel();
    let ext_home = home.clone();
    let ext_config = config.clone();
    std::thread::spawn(move || {
        drop(ext_done.send(extensions::SessionExtensions::load(
            &ext_home,
            &ext_config,
            fakes::clock::FakeClock::new(),
            std::sync::Arc::new(tools::PathLocks::new()),
            None,
        )));
    });
    let session_extensions = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&ext_ready)
        .expect("extensions loaded in time");
    let lua = session_extensions
        .lua_providers()
        .iter()
        .find(|(_, provider)| provider.name() == "qp")
        .map(|(_, provider)| std::sync::Arc::clone(provider))
        .expect("the package registers its provider");
    // `choose_reviewer` calls `connect`, which can wait on Lua registration,
    // so it runs on its own thread under a deadline as well.
    let (rev_done, rev_ready) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let session = providers.resolve("qp/m1").unwrap();
        let here = here();
        let mut lookup = |_: &config::ProviderData| {
            Ok((
                "work".to_owned(),
                crate::lua_providers::Access::new(
                    Some(&lua),
                    (Some(contract::Secret::new("key".to_owned())), None),
                ),
            ))
        };
        drop(rev_done.send(crate::choose_reviewer(
            &providers,
            &config,
            &session,
            &here,
            &mut lookup,
            None,
        )));
    });
    let reviewer = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rev_ready)
        .expect("choose_reviewer ended in time")
        .unwrap()
        .0;
    assert_eq!(reviewer.model.reference, "qp/m2");
    let _call = reviewer.provider.call(&reviewer_request());
    assert!(
        server.await_requests(1, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the reviewer call never fetched quota"
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(paths, ["/work"]);
}

fn reviewer_request() -> contract::provider::ModelRequest {
    contract::provider::ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: contract::events::CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: Vec::new(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
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

/// The reviewer reference for `session` under `overrides`.
fn reference(
    pair: &Pair,
    session: &str,
    overrides: &[&str],
) -> Result<String, contract::shapes::Failure> {
    let config = pair_config(pair, overrides);
    let providers = pair_providers(pair);
    let resolved = providers.resolve(session).unwrap();
    crate::reviewer_reference(&config, &resolved)
}

#[test]
fn explicit_reviewer_model_wins_over_provider_and_package_picks() {
    let pair = pair("fiber-reviewer-explicit");
    assert_eq!(
        reference(
            &pair,
            "alpha/a-session",
            &[
                "reviewer.model=alpha/a-want",
                "providers.alpha.reviewer_model=a-other",
            ],
        )
        .unwrap(),
        "alpha/a-want"
    );
}

#[test]
fn provider_choice_wins_over_the_package_default() {
    let pair = pair("fiber-reviewer-provider");
    assert_eq!(
        reference(
            &pair,
            "alpha/a-session",
            &["providers.alpha.reviewer_model=a-want"]
        )
        .unwrap(),
        "alpha/a-want"
    );
}

#[test]
fn package_default_reviews_when_nothing_is_saved() {
    let pair = pair("fiber-reviewer-package");
    assert_eq!(
        reference(&pair, "alpha/a-session", &[]).unwrap(),
        "alpha/a-pkg"
    );
}

#[test]
fn no_source_is_no_model_with_the_loop_message() {
    let pair = pair("fiber-reviewer-none");
    let failure = reference(&pair, "beta/b-session", &[]).unwrap_err();
    assert_eq!(failure.code, contract::ErrorCode::NoModel);
    assert_eq!(failure.message, r#loop::NO_MODEL_MESSAGE);
}

#[test]
fn project_provider_value_overrides_global_and_repository_is_ignored() {
    let pair = pair("fiber-reviewer-layers");
    std::fs::write(
        pair.home.join("config.json"),
        r#"{"providers": {"alpha": {"reviewer_model": "a-other"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(pair.home.join("projects/test")).unwrap();
    std::fs::write(
        pair.home.join("projects/test/config.json"),
        r#"{"providers": {"alpha": {"reviewer_model": "a-want"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(pair.workspace.join(".fiber")).unwrap();
    std::fs::write(
        pair.workspace.join(".fiber/config.json"),
        r#"{"providers": {"alpha": {"reviewer_model": "a-session"}}}"#,
    )
    .unwrap();
    let config = pair_config(&pair, &[]);
    assert_eq!(
        config.notices().len(),
        1,
        "expected the repository refusal, got {:?}",
        config.notices()
    );
    let notice = &config.notices()[0];
    assert_eq!(notice.code, contract::ErrorCode::ConfigKeyIgnored);
    assert!(
        notice.message.contains("`providers.alpha.reviewer_model`"),
        "{}",
        notice.message
    );
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    assert_eq!(
        crate::reviewer_reference(&config, &session).unwrap(),
        "alpha/a-want"
    );
}

#[test]
fn the_lookup_follows_the_session_provider_not_its_credential_name() {
    let pair = pair("fiber-reviewer-session-provider");
    let overrides = [
        "providers.alpha.reviewer_model=a-want",
        "providers.beta.reviewer_model=b-want",
    ];
    assert_eq!(
        reference(&pair, "alpha/a-session", &overrides).unwrap(),
        "alpha/a-want"
    );
    assert_eq!(
        reference(&pair, "beta/b-session", &overrides).unwrap(),
        "beta/b-want",
        "both providers share one credential name, yet each resolves its own pick"
    );
}

#[test]
fn slash_ids_resolve_verbatim() {
    let pair = pair("fiber-reviewer-slash");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=org/fast"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    assert_eq!(
        crate::reviewer_reference(&config, &session).unwrap(),
        "alpha/org/fast"
    );
    let here = here();
    let mut lookup = |_: &config::ProviderData| {
        Ok((
            "default".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let (reviewer, _) =
        crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None).unwrap();
    assert_eq!(reviewer.model.reference, "alpha/org/fast");
}

#[test]
fn colon_ids_prefix_verbatim() {
    let pair = pair("fiber-reviewer-colon");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=v:2"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    assert_eq!(
        crate::reviewer_reference(&config, &session).unwrap(),
        "alpha/v:2",
        "the local ID is prefixed whole, never split on `:`"
    );
    let here = here();
    let mut lookup = |_: &config::ProviderData| {
        Ok((
            "default".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let (reviewer, _) =
        crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None).unwrap();
    assert_eq!(reviewer.model.reference, "alpha/v:2");
}

#[test]
fn an_invalid_saved_model_fails_instead_of_falling_back() {
    let pair = pair("fiber-reviewer-invalid");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=nope"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    // The reference still names the saved pick: only resolution refuses it.
    assert_eq!(
        crate::reviewer_reference(&config, &session).unwrap(),
        "alpha/nope"
    );
    let expected = providers.resolve("alpha/nope").unwrap_err();
    let here = here();
    let mut lookup = |_: &config::ProviderData| {
        Ok((
            "default".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let failure = reviewer_failed(crate::choose_reviewer(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        None,
    ));
    assert_eq!(failure.code, expected.code());
    assert_eq!(failure.message, expected.to_string());
}

#[test]
fn choose_reviewer_reads_the_reviewers_provider_under_its_label() {
    let pair = pair("fiber-reviewer-lookup");
    // An explicit cross-provider pick: the lookup still runs for the
    // reviewer's provider, not the session's.
    let config = pair_config(&pair, &["reviewer.model=beta/b-want"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    let here = here();
    let mut seen = Vec::new();
    let mut lookup = |provider: &config::ProviderData| {
        seen.push(provider.name.clone());
        Ok((
            "work".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let (reviewer, _) =
        crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None).unwrap();
    assert_eq!(reviewer.model.reference, "beta/b-want");
    assert_eq!(
        seen,
        ["beta"],
        "the lookup runs once, for the reviewer's provider"
    );
}

#[test]
fn a_native_debug_reviewer_keeps_its_factory_and_none_or_info_keeps_none() {
    let pair = pair("fiber-reviewer-factory");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=a-want"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    let here = here();
    let mut lookup = |_: &config::ProviderData| {
        Ok((
            "default".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let clock = fakes::clock::FakeClock::new();
    let debug = std::sync::Arc::new(log::diag::Diag::new(
        &pair.home,
        log::diag::Process::Session,
        log::diag::Level::Debug,
        clock.clone(),
    ));
    let (_, observed) = crate::choose_reviewer(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        Some(&debug),
    )
    .unwrap();
    assert!(
        observed.is_some(),
        "a native debug reviewer carries a factory"
    );
    let (_, unobserved) =
        crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup, None).unwrap();
    assert!(unobserved.is_none());
    let info = std::sync::Arc::new(log::diag::Diag::new(
        &pair.home,
        log::diag::Process::Session,
        log::diag::Level::Info,
        clock,
    ));
    let (_, quiet) = crate::choose_reviewer(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        Some(&info),
    )
    .unwrap();
    assert!(quiet.is_none());
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
