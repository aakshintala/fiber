//! Choosing step 7's limits from configuration (`docs/configuration.md`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use crate::settings::block_limits;
use crate::test_support;

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
    let reviewer =
        crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup).unwrap();
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
            crate::choose_reviewer(&providers, &config, &session, &here, &mut lookup).unwrap();
        assert_eq!(reviewer.model.reference, reference);
        assert_eq!(reviewer.provider.cost_lookup().is_some(), carries);
    }
}

/// How long a test waits for the quota fetch.
const QUOTA_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

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
        home.join("extensions/qp/init.lua"),
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
    let session_extensions = fakes::Deadline::after(QUOTA_WAIT)
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
        )));
    });
    let reviewer = fakes::Deadline::after(QUOTA_WAIT)
        .recv(&rev_ready)
        .expect("choose_reviewer ended in time")
        .unwrap();
    assert_eq!(reviewer.model.reference, "qp/m2");
    let _call = reviewer.provider.call(&reviewer_request());
    assert!(
        server.await_requests(1, QUOTA_WAIT),
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
