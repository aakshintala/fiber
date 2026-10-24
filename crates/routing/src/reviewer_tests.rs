//! The reviewer lookup (`docs/permissions.md`, "How it runs"): which model
//! reviews, in what order its checks fail, and the provider built for it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use contract::shapes::Failure;
use fakes::MUST_SUCCEED_WITHIN;

use super::Chosen;
use crate::lua_providers::Access;
use crate::test_support::Level;
use crate::{Diagnostics, Here};

fn here() -> Here {
    Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
    }
}

/// The reviewer a choice names, which must exist.
fn chosen(choice: Result<Option<Chosen<'_>>, Failure>) -> Chosen<'_> {
    choice.unwrap().expect("a reviewer is named")
}

/// A failed choice: `Chosen` is no `Debug`, so no `unwrap_err`.
fn choice_failed(choice: Result<Option<Chosen<'_>>, Failure>) -> Failure {
    match choice {
        Ok(_) => panic!("the reviewer was chosen"),
        Err(failure) => failure,
    }
}

/// A lookup that grants a key under `label` and counts its calls.
fn counting<'a>(
    label: &'a str,
    calls: &'a std::cell::Cell<u32>,
) -> impl FnMut(&config::ProviderData) -> Result<(String, Access), Failure> + 'a {
    move |_| {
        calls.set(calls.get() + 1);
        Ok((
            label.to_owned(),
            Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    }
}

/// The reviewer's provider carries the lookup of the Lua provider its
/// `Access` names, and none without one (`docs/model-routing.md`, "Cost").
#[test]
fn the_reviewer_connects_with_the_lua_provider_its_access_names() {
    let root = fakes::TempDir::new("fiber-reviewer-lua-cost");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    crate::test_support::install_extension(
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
        "fiber.provider(\"qp\", { cost = { timeout = 1000, run = function() return 0 end } })\n",
    )
    .unwrap();
    let reference = "qp/m2";
    let config = crate::test_support::load(
        &home,
        &workspace,
        "test",
        [format!("reviewer.model={reference}")],
    );
    let (providers, _notices) = extensions::Providers::load(&home).unwrap();
    // `SessionExtensions::load` can wait on Lua registration, so it runs
    // under a deadline instead of hanging the test.
    let (load_home, load_config) = (home.clone(), config.clone());
    let session_extensions = fakes::within(
        "loading the extensions",
        fakes::Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            extensions::SessionExtensions::load(
                &load_home,
                &load_config,
                fakes::clock::FakeClock::new(),
                std::sync::Arc::new(fakes::NoLock),
                None,
            )
        },
    );
    let lua = session_extensions
        .lua_providers()
        .iter()
        .find(|(_, provider)| provider.name() == "qp")
        .map(|(_, provider)| std::sync::Arc::clone(provider))
        .expect("the package registers its provider");
    let session = providers.resolve("qp/m1").unwrap();
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
        let reviewer = chosen(super::choose(
            &providers,
            &config,
            &session,
            &here,
            &mut lookup,
            None,
        ));
        assert_eq!(reviewer.model.reference(), reference);
        assert_eq!(reviewer.provider.cost_lookup().is_some(), carries);
    }
}

/// A model call through the reviewer `choose` builds fetches quota
/// for the label its lookup returned, which proves `choose` hands
/// that label to `connect` (`docs/tools.md`, "Provider quota").
#[test]
fn the_reviewer_fetches_quota_for_the_lookup_label() {
    let root = fakes::TempDir::new("fiber-reviewer-quota");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, "{\"windows\": []}")]).unwrap();
    crate::test_support::install_extension(
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
    let config = crate::test_support::load(&home, &workspace, "test", ["reviewer.model=qp/m2"]);
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
            std::sync::Arc::new(fakes::NoLock),
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
    // `choose` calls `connect`, which can wait on Lua registration,
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
        // The chosen reviewer borrows `providers`, so only its reference and
        // provider cross the thread.
        let chosen = super::choose(&providers, &config, &session, &here, &mut lookup, None)
            .map(|chosen| chosen.map(|chosen| (chosen.model.reference(), chosen.provider)));
        drop(rev_done.send(chosen));
    });
    let reviewer = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rev_ready)
        .expect("choose ended in time")
        .unwrap()
        .expect("a reviewer is named");
    assert_eq!(reviewer.0, "qp/m2");
    let _call = reviewer.1.call(&reviewer_request());
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
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
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
    crate::test_support::install_extension(
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
    crate::test_support::load(&pair.home, &pair.workspace, "test", overrides)
}

fn pair_providers(pair: &Pair) -> extensions::Providers {
    let (providers, _notices) = extensions::Providers::load(&pair.home).unwrap();
    providers
}

/// The reviewer reference for `session` under `overrides`.
fn reference(pair: &Pair, session: &str, overrides: &[&str]) -> Option<String> {
    let config = pair_config(pair, overrides);
    let providers = pair_providers(pair);
    let resolved = providers.resolve(session).unwrap();
    super::reference(&config, &resolved)
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
    assert_eq!(super::reference(&config, &session).unwrap(), "alpha/a-want");
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
        super::reference(&config, &session).unwrap(),
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
    let reviewer = chosen(super::choose(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        None,
    ));
    assert_eq!(reviewer.model.reference(), "alpha/org/fast");
}

#[test]
fn colon_ids_prefix_verbatim() {
    let pair = pair("fiber-reviewer-colon");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=v:2"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    assert_eq!(
        super::reference(&config, &session).unwrap(),
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
    let reviewer = chosen(super::choose(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        None,
    ));
    assert_eq!(reviewer.model.reference(), "alpha/v:2");
}

#[test]
fn an_invalid_saved_model_fails_instead_of_falling_back() {
    let pair = pair("fiber-reviewer-invalid");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=nope"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    // The reference still names the saved pick: only resolution refuses it.
    assert_eq!(super::reference(&config, &session).unwrap(), "alpha/nope");
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
    let failure = choice_failed(super::choose(
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
fn choose_reads_the_reviewers_provider_under_its_label() {
    let pair = pair("fiber-reviewer-lookup");
    // An explicit cross-provider pick resolves the named reviewer.
    let config = pair_config(&pair, &["reviewer.model=beta/b-want"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    let here = here();
    let mut lookup = |_: &config::ProviderData| {
        Ok((
            "work".to_owned(),
            crate::lua_providers::Access {
                key: Some(contract::Secret::new("key".to_owned())),
                signer: None,
                lua: None,
            },
        ))
    };
    let reviewer = chosen(super::choose(
        &providers,
        &config,
        &session,
        &here,
        &mut lookup,
        None,
    ));
    assert_eq!(reviewer.model.reference(), "beta/b-want");
}

#[test]
fn no_source_is_no_reference_and_no_reviewer() {
    let pair = pair("fiber-reviewer-none");
    assert_eq!(reference(&pair, "beta/b-session", &[]), None);
    let config = pair_config(&pair, &[]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("beta/b-session").unwrap();
    assert!(
        super::model(&providers, &config, &session)
            .unwrap()
            .is_none()
    );
    let calls = std::cell::Cell::new(0);
    let mut lookup = counting("default", &calls);
    let choice = super::choose(&providers, &config, &session, &here(), &mut lookup, None);
    assert!(choice.unwrap().is_none());
    assert_eq!(calls.get(), 0, "no reviewer reads no credential");
}

/// The window check for a model declaring `window`. The registry leaves
/// out a model with no positive window when it loads, so the check is
/// reached here with a hand-built model.
fn window_of(window: serde_json::Value) -> Result<u64, Failure> {
    let mut model = serde_json::json!({
        "id": "m",
        "protocol": "openai-responses",
        "base_url": "http://127.0.0.1:9/v1",
    });
    if !window.is_null() {
        model["context_window"] = window;
    }
    let data: config::ProviderData =
        serde_json::from_value(serde_json::json!({"name": "p", "models": [model]})).unwrap();
    super::context_window(&extensions::Model {
        provider: &data,
        model: &data.models[0],
        thinking: None,
    })
}

#[test]
fn a_missing_or_zero_context_window_is_no_model_with_the_extensions_text() {
    for window in [serde_json::Value::Null, serde_json::json!(0)] {
        let failure = window_of(window).unwrap_err();
        assert_eq!(failure.code, contract::ErrorCode::NoModel);
        assert_eq!(
            failure.message,
            "The model `p/m` declares no `context_window`."
        );
        assert_eq!(failure.retry_after_ms, None);
        assert_eq!(failure.provider, None);
    }
}

#[test]
fn a_positive_context_window_is_the_models_window() {
    assert_eq!(window_of(serde_json::json!(1)).unwrap(), 1);
    assert_eq!(window_of(serde_json::json!(1_000)).unwrap(), 1_000);
}

#[test]
fn a_failing_lookup_fails_the_choice_before_any_connection() {
    let pair = pair("fiber-reviewer-lookup-fails");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=a-want"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    let mut lookup = |_: &config::ProviderData| -> Result<(String, Access), Failure> {
        Err(Failure {
            code: contract::ErrorCode::CredentialMissing,
            message: "no key".to_owned(),
            retry_after_ms: None,
            provider: None,
        })
    };
    let failure = choice_failed(super::choose(
        &providers,
        &config,
        &session,
        &here(),
        &mut lookup,
        None,
    ));
    assert_eq!(failure.code, contract::ErrorCode::CredentialMissing);
    assert_eq!(failure.message, "no key");
}

/// The factory `choose` keeps for the reviewer when the writer is at debug
/// (`debug`) and diagnostics are on (`on`), as `Diagnostics` carries them.
fn factory(debug: bool, on: bool) -> bool {
    let pair = pair("fiber-reviewer-factory");
    let config = pair_config(&pair, &["providers.alpha.reviewer_model=a-want"]);
    let providers = pair_providers(&pair);
    let session = providers.resolve("alpha/a-session").unwrap();
    let calls = std::cell::Cell::new(0);
    let mut lookup = counting("default", &calls);
    let diag = Diagnostics {
        log: std::sync::Arc::new(Level { debug }),
        timings: std::sync::Arc::new(|_, _, _| None),
    };
    let diag = on.then_some(&diag);
    chosen(super::choose(
        &providers,
        &config,
        &session,
        &here(),
        &mut lookup,
        diag,
    ))
    .observe
    .is_some()
}

#[test]
fn a_native_debug_reviewer_keeps_its_factory_and_none_or_info_keeps_none() {
    assert!(
        factory(true, true),
        "a native debug reviewer carries a factory"
    );
    assert!(!factory(true, false), "no diagnostics, no factory");
    assert!(!factory(false, true), "info keeps no factory");
}
