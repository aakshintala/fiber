//! A provider's Lua against the fake server (`docs/model-routing.md`, "Model
//! discovery", "Signing a request" and "Credentials"; `docs/configuration.md`,
//! "A provider's data"): the fixture extension's `models()`, `credential()`
//! and `sign()`.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]
#![allow(
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::{Setup, pair, script_provider, write};
use config::{Secret, store_secret};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::signing::{SignRequest, Signer};
use extensions::{Error, LuaExtension, LuaProvider, ModelsArg, REFRESH_BEFORE};
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use fakes::{ProviderServer, Response, fingerprint};
use serde_json::json;

/// How long a test waits for one call, or for a background refresh.
const WAIT: Duration = Duration::from_secs(5);

// debt: 4 copies of this helper across the extensions test binaries, move into common/mod.rs once group 9C1 merges
/// The thread parked at `at`: exactly one thread waits there.
#[track_caller]
fn thread_parked_at(clock: &FakeClock, at: Instant) -> std::thread::ThreadId {
    let mark = clock
        .mark_parked(at, WAIT)
        .unwrap_or_else(|| panic!("waited for a park at {at:?} within {WAIT:?}"));
    let threads = mark.threads();
    assert_eq!(
        threads.len(),
        1,
        "only one thread parks at {at:?}: {threads:?}"
    );
    match threads.into_iter().next() {
        Some(id) => id,
        None => panic!("one parked thread"),
    }
}

fn provider_file(lua: &str) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("lazy-provider");
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("providers/p.lua"), lua).unwrap();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        FakeClock::new(),
    ));
    (dir, ext)
}

#[test]
fn provider_only_file_lists_functions_on_first_use() {
    let (_dir, ext) =
        provider_file("fiber.provider('p', {models={timeout=100, run=function() return {} end}})");
    assert!(!ext.is_running());
    let functions = fakes::within(
        "ext.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || ext.provider_functions("p"),
    )
    .unwrap();
    assert_eq!(functions, ["models"]);
}

#[test]
fn provider_file_refuses_wrong_duplicate_missing_and_tool_registrations() {
    for (lua, reason) in [
        ("fiber.provider('q', {})", "not `q`"),
        (
            "fiber.provider('p', {}) pcall(function() fiber.provider('p', {}) end)",
            "exactly once",
        ),
        ("local x = 1", "registers no provider `p`"),
        ("fiber.tool('t', {})", "init.lua"),
    ] {
        let (_dir, ext) = provider_file(lua);
        let err = fakes::within(
            "ext.provider_functions",
            fakes::MUST_SUCCEED_WITHIN,
            move || ext.provider_functions("p"),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        let Error::Lua { message, .. } = err else {
            panic!("{err:?}")
        };
        assert!(message.contains("providers/p.lua"), "{message}");
        assert!(message.contains(reason), "{message}");
    }
}

#[test]
fn a_provider_file_cannot_hide_a_tool_registration_refusal_with_pcall() {
    let (_dir, ext) = provider_file(
        "fiber.provider('p', {})\n local ok = pcall(fiber.tool, 't', {})\n assert(not ok)",
    );
    let error = fakes::within(
        "ext.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || ext.provider_functions("p"),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionFailed);
    let Error::Lua { message, .. } = error else {
        panic!("{error:?}")
    };
    assert!(message.contains("providers/p.lua"), "{message}");
    assert!(message.contains("fiber.tool"), "{message}");
    assert!(message.contains("only while `init.lua` runs"), "{message}");
}

#[test]
fn a_provider_file_cannot_hide_a_search_backend_registration_refusal_with_pcall() {
    let (_dir, ext) = provider_file(
        "fiber.provider('p', {})\n local ok = pcall(fiber.search_backend, 's', {})\n assert(not ok)",
    );
    let error = fakes::within(
        "ext.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || ext.provider_functions("p"),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionFailed);
    let Error::Lua { message, .. } = error else {
        panic!("{error:?}")
    };
    assert!(message.contains("providers/p.lua"), "{message}");
    assert!(message.contains("fiber.search_backend"), "{message}");
    assert!(message.contains("only while `init.lua` runs"), "{message}");
}

#[test]
fn absent_provider_file_starts_no_vm() {
    let dir = fakes::TempDir::new("absent-provider");
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        FakeClock::new(),
    ));
    let copy = Arc::clone(&ext);
    assert_eq!(
        fakes::within(
            "copy.provider_functions",
            fakes::MUST_SUCCEED_WITHIN,
            move || copy.provider_functions("none")
        )
        .unwrap(),
        Vec::<String>::new()
    );
    assert!(!ext.is_running());
}

#[test]
fn oversized_provider_file_names_the_file() {
    let (_dir, ext) = provider_file(&" ".repeat(extensions::MEMORY_CAP + 1));
    let err = fakes::within(
        "ext.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || ext.provider_functions("p"),
    )
    .unwrap_err();
    let Error::Lua { message, .. } = err else {
        panic!("{err:?}")
    };
    assert!(message.contains("providers/p.lua"), "{message}");
    assert!(message.contains("memory cap"), "{message}");
}

#[test]
fn first_use_load_failure_is_extension_failed_not_credential_failed() {
    let (_dir, ext) = provider_file("error('broken at load')");
    let provider = LuaProvider::new(ext, "p");
    let err = fakes::within(
        "provider.credential_value",
        fakes::MUST_SUCCEED_WITHIN,
        move || provider.credential_value(&pair("p", "p")),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    assert!(
        matches!(err, Error::Lua { message, .. } if message.contains("providers/p.lua:1:") && message.contains("broken at load"))
    );
}

#[test]
fn credential_callback_failure_keeps_credential_failed() {
    let (_dir, ext) = provider_file(
        "fiber.provider('p', {credential={timeout=100, run=function() error('broken credential') end}})",
    );
    let provider = LuaProvider::new(ext, "p");
    let err = fakes::within(
        "provider.credential_value",
        fakes::MUST_SUCCEED_WITHIN,
        move || provider.credential_value(&pair("p", "p")),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    assert!(matches!(err, Error::Credential(_)));
    assert!(err.to_string().contains("providers/p.lua:1:"));
}

/// The fixture's provider, with its server's address and key stored as
/// secrets in a fresh Fiber home.
fn fixture(setup: &Setup, server: &ProviderServer) -> Arc<LuaProvider> {
    fixture_on(setup, server, FakeClock::new())
}

fn fixture_on(setup: &Setup, server: &ProviderServer, clock: Arc<FakeClock>) -> Arc<LuaProvider> {
    let home = setup.home();
    store_secret(&home, "fixture.url", &Secret::new(server.url())).unwrap();
    store_secret(&home, "fixture.api_key", &Secret::new("k1".into())).unwrap();
    let extension = Arc::new(
        LuaExtension::new("fixture", fakes::lua_fixture(), &home, clock)
            .with_secrets(vec!["fixture.url".into(), "fixture.api_key".into()]),
    );
    LuaProvider::new(extension, "fixture")
}

fn listing(ids: &[&str]) -> Response {
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({ "id": id, "context_length": 1000 }))
        .collect();
    Response::status(200, json!({ "data": data }).to_string())
}

fn token(value: &str, expires_in: Duration) -> Response {
    // A fresh fake clock's wall is the extension's wall until a test advances it.
    let expires = FakeClock::new().wall().duration_since(UNIX_EPOCH).unwrap() + expires_in;
    Response::status(
        200,
        json!({ "access_token": value, "expires_at": expires.as_secs() }).to_string(),
    )
}

fn ids(models: &[config::ModelData]) -> Vec<&str> {
    models.iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn models_runs_when_there_is_no_cached_copy_and_its_list_is_cached() {
    let setup = Setup::new();
    let server = ProviderServer::start([listing(&["m1", "m2"])]).unwrap();
    let provider = fixture(&setup, &server);
    assert!(
        !provider.has_model_cache(),
        "no copy before the first discovery"
    );
    let models = fakes::within("provider.models", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.models(&ModelsArg::default())
    })
    .unwrap();
    assert_eq!(ids(&models), ["m1", "m2"]);
    assert!(provider.has_model_cache(), "discovery writes the copy");
    assert_eq!(models[0].base_url, format!("{}/v1", server.url()));
    assert_eq!(models[0].context_window, Some(1000));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/models");
    assert_eq!(
        requests[0].header("authorization"),
        Some(fingerprint("Bearer k1").as_str())
    );
    let cached = config::read_model_cache(&setup.home(), "fixture").unwrap();
    assert_eq!(cached.as_deref(), Some(models.as_slice()));

    // Asked again, it serves what it has.
    assert_eq!(
        ids(&provider.models(&ModelsArg::default()).unwrap()),
        ["m1", "m2"]
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_cached_list_is_served_without_running_lua_until_the_refresh_returns() {
    let setup = Setup::new();
    let server = ProviderServer::start([listing(&["new"])]).unwrap();
    let provider = fixture(&setup, &server);
    let old = json!([{ "id": "old", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000 }]);
    write(
        &setup.home().join("cache/models/fixture.json"),
        &old.to_string(),
    );

    assert_eq!(
        ids(&provider.models(&ModelsArg::default()).unwrap()),
        ["old"]
    );
    assert!(server.requests().is_empty(), "the cached copy is served");

    let refresh = provider.refresh(None, ModelsArg::default()).unwrap();
    let refreshed = fakes::within("refresh.join", fakes::MUST_SUCCEED_WITHIN, move || {
        refresh.join().unwrap()
    })
    .unwrap();
    assert_eq!(ids(&refreshed), ["new"]);
    assert_eq!(
        ids(&provider.models(&ModelsArg::default()).unwrap()),
        ["new"]
    );
    let cached = config::read_model_cache(&setup.home(), "fixture")
        .unwrap()
        .unwrap();
    assert_eq!(ids(&cached), ["new"]);
}

#[test]
fn a_models_call_that_fails_or_returns_no_model_list_caches_nothing() {
    for (reply, bad_return) in [
        ("not json", false),
        (r#"{"data":[{"context_length":1}]}"#, true),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start([Response::status(200, reply)]).unwrap();
        let provider = fixture(&setup, &server);
        let err = fakes::within("provider.models", fakes::MUST_SUCCEED_WITHIN, move || {
            provider.models(&ModelsArg::default())
        })
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        assert_eq!(
            matches!(err, Error::BadReturn { .. }),
            bad_return,
            "{err:?}"
        );
        assert_eq!(
            config::read_model_cache(&setup.home(), "fixture").unwrap(),
            None
        );
    }
}

#[test]
fn a_token_far_from_expiry_is_reused() {
    let setup = Setup::new();
    let server = ProviderServer::start([token("t1", Duration::from_secs(3600))]).unwrap();
    let provider = fixture(&setup, &server);
    for _ in 0..3 {
        let provider = Arc::clone(&provider);
        assert_eq!(
            fakes::within("provider.token", fakes::MUST_SUCCEED_WITHIN, move || {
                provider.token(&pair(provider.name(), "default"))
            })
            .unwrap()
            .expose(),
            "t1"
        );
    }
    assert_eq!(server.requests().len(), 1);
    let request = &server.requests()[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/token")
    );
    assert_eq!(request.body, br#"{"key":"k1"}"#);
}

#[test]
fn a_token_within_five_minutes_of_expiry_is_refreshed_off_the_request_path() {
    assert_eq!(REFRESH_BEFORE, Duration::from_secs(300));
    let setup = Setup::new();
    let server = ProviderServer::start([
        token("t1", Duration::from_secs(3600)),
        // Far past the advanced clock, so `t2` is not itself due: a poll that
        // sees it starts no further refresh, and the count stays at two.
        token("t2", Duration::from_secs(2 * 3600)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = fixture_on(&setup, &server, clock.clone());
    let first = Arc::clone(&provider);
    assert_eq!(
        fakes::within("first.token", fakes::MUST_SUCCEED_WITHIN, move || first
            .token(&pair(first.name(), "default")))
        .unwrap()
        .expose(),
        "t1"
    );
    assert_eq!(server.requests().len(), 1);
    // Into the refresh window, still short of expiry. No code under test
    // waits on the clock here; the token call returned (ruling 6179406086).
    clock.advance(
        Duration::from_secs(3600)
            .checked_sub(REFRESH_BEFORE)
            .unwrap(),
    );
    let second = Arc::clone(&provider);
    assert_eq!(
        fakes::within("second.token", fakes::MUST_SUCCEED_WITHIN, move || second
            .token(&pair(second.name(), "default")))
        .unwrap()
        .expose(),
        "t1"
    );
    assert!(
        server.await_requests(2, WAIT),
        "waited for the refresh request"
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match provider.token(&pair(provider.name(), "default")) {
                Ok(secret) if secret.expose() == "t2" => {
                    match tx.send(()) {
                        Ok(()) | Err(mpsc::SendError(())) => {}
                    }
                    return;
                }
                _ => std::thread::yield_now(),
            }
        }
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the refreshed token");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn an_expired_token_just_returned_is_an_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([token("t1", Duration::ZERO)]).unwrap();
    let provider = fixture(&setup, &server);
    let err = fakes::within("provider.token", fakes::MUST_SUCCEED_WITHIN, move || {
        provider.token(&pair(provider.name(), "default"))
    })
    .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Credential(inner) if matches!(inner.as_ref(), Error::BadReturn { .. })
        ),
        "{err:?}"
    );
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    assert!(err.to_string().contains("already expired"), "{err}");
}

#[test]
fn a_credential_with_no_usable_expiry_is_an_error() {
    for body in [
        r#"{"access_token":"t1"}"#,
        r#"{"access_token":"t1","expires_at":"soon"}"#,
        r#"{"access_token":"t1","expires_at":null}"#,
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start([Response::status(200, body)]).unwrap();
        let provider = fixture(&setup, &server);
        let err = fakes::within("provider.token", fakes::MUST_SUCCEED_WITHIN, move || {
            provider.token(&pair(provider.name(), "default"))
        })
        .unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Credential(inner) if matches!(inner.as_ref(), Error::BadReturn { .. })
            ),
            "{body}: {err:?}"
        );
        assert_eq!(err.code(), ErrorCode::CredentialFailed);
        assert!(err.to_string().contains("expires_at"), "{err}");
    }
}

#[test]
fn a_credential_with_no_token_is_credential_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, r#"{"expires_at": 1}"#)]).unwrap();
    let provider = fixture(&setup, &server);
    let err = fakes::within("provider.token", fakes::MUST_SUCCEED_WITHIN, move || {
        provider.token(&pair(provider.name(), "default"))
    })
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    let Error::Credential(inner) = &err else {
        panic!("{err:?}")
    };
    let Error::BadReturn {
        extension,
        callback,
        ..
    } = inner.as_ref()
    else {
        panic!("{err:?}")
    };
    assert_eq!(
        (extension.as_str(), callback.as_str()),
        ("fixture", "fixture.credential")
    );
}

#[test]
fn sign_returns_a_table_of_headers_or_nothing() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    for (name, returns) in [
        ("empty", "{}"),
        ("none", "nil"),
        ("list", "{ \"a\" }"),
        ("number", "1"),
    ] {
        write(
            &dir.join("providers").join(format!("{name}.lua")),
            &format!(
                "fiber.provider(\"{name}\", {{ sign = {{ timeout = 1000, run = function() return {returns} end }} }})\n"
            ),
        );
    }
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        fakes::clock::FakeClock::new(),
    ));
    for (name, ok) in [
        ("empty", true),
        ("none", true),
        ("list", false),
        ("number", false),
    ] {
        let provider = LuaProvider::new(Arc::clone(&extension), name);
        let signed = fakes::within("provider.sign", fakes::MUST_SUCCEED_WITHIN, move || {
            provider.sign(&SignRequest {
                method: "POST",
                url: "http://x/",
                headers: &[],
                body: b"",
            })
        });
        match signed {
            Ok(headers) => assert!(ok && headers.is_empty(), "{name}: {headers:?}"),
            Err(why) => assert!(
                !ok && why.to_string().contains("a table of headers"),
                "{name}: {why}"
            ),
        }
    }
}

/// `sign()` is handed the extension clock's wall time in whole Unix
/// seconds, which a scheme such as AWS SigV4 signs over
/// (`docs/model-routing.md`, "Signing a request").
#[test]
fn sign_sees_the_extension_clocks_time_in_unix_seconds() {
    let dir = fakes::TempDir::new("sign-time");
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(
        dir.path().join("providers/p.lua"),
        "fiber.provider('p', {sign={timeout=1000, run=function(arg) \
         return {['x-time'] = string.format('%d', arg.time)} end}})",
    )
    .unwrap();
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        Arc::clone(&clock) as Arc<dyn Clock>,
    ));
    let provider = LuaProvider::new(ext, "p");
    let sign = |provider: &Arc<LuaProvider>| {
        let provider = Arc::clone(provider);
        fakes::within("provider.sign", fakes::MUST_SUCCEED_WITHIN, move || {
            provider.sign(&SignRequest {
                method: "POST",
                url: "https://example.test/",
                headers: &[],
                body: b"",
            })
        })
        .unwrap()
    };
    assert_eq!(
        sign(&provider),
        [("x-time".to_owned(), "1700000000".to_owned())]
    );
    clock.advance(Duration::from_millis(61_500));
    assert_eq!(
        sign(&provider),
        [("x-time".to_owned(), "1700000061".to_owned())]
    );
}

#[test]
fn sign_sees_the_bodys_hash_never_the_body_and_adds_headers() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let provider = fixture(&setup, &server);
    let body = br#"{"model":"m1"}"#;
    let url = format!("{}/v1/responses", server.url());
    let signer = Arc::clone(&provider);
    let call_url = url.clone();
    let headers = fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &call_url,
            headers: &[("x-client".to_owned(), "fiber".to_owned())],
            body,
        })
    })
    .unwrap();
    let get = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    let hash = hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref());
    assert_eq!(get("x-fixture-content-sha256"), hash);
    assert_eq!(get("x-fixture-saw"), "body_sha256,headers,method,time,url");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"fixture-secret");
    let text = format!("POST\n{url}\n{hash}");
    assert_eq!(
        get("x-fixture-signature"),
        hex(ring::hmac::sign(&key, text.as_bytes()).as_ref())
    );
    assert_eq!(headers.len(), 3);
}

/// A background `models()` stuck in `host.http` must not hold the extension's
/// thread: `sign()` returns inside its own deadline (`docs/extensions.md`,
/// "A host call suspends the code that made it").
#[test]
fn sign_returns_while_a_background_refresh_is_stuck_on_http() {
    let setup = Setup::new();
    let server = ProviderServer::start([listing(&["m1", "m2"])]).unwrap();
    server.hold();
    let home = setup.home();
    store_secret(&home, "fixture.url", &Secret::new(server.url())).unwrap();
    store_secret(&home, "fixture.api_key", &Secret::new("k1".into())).unwrap();
    let extension = Arc::new(
        LuaExtension::new(
            "fixture",
            fakes::lua_fixture(),
            &home,
            fakes::clock::FakeClock::new(),
        )
        .with_secrets(vec!["fixture.url".into(), "fixture.api_key".into()]),
    );
    let provider = LuaProvider::new(extension, "fixture");
    let refresh = provider.refresh(None, ModelsArg::default()).unwrap();
    assert!(
        server.await_requests(1, WAIT),
        "waited for models() to reach the server"
    );
    let signer = Arc::clone(&provider);
    let headers = fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: "http://127.0.0.1/v1/responses",
            headers: &[],
            body: b"{}",
        })
    })
    .unwrap();
    assert_eq!(headers.len(), 3);
    assert!(
        config::read_model_cache(&home, "fixture")
            .unwrap()
            .is_none(),
        "the refresh is still suspended"
    );
    drop(refresh);
}

#[test]
fn the_fixture_registers_each_provider_function() {
    let setup = Setup::new();
    let extension = Arc::new(LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        setup.home(),
        fakes::clock::FakeClock::new(),
    ));
    let functions = fakes::within(
        "extension.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || extension.provider_functions("fixture"),
    )
    .unwrap();
    assert_eq!(functions, ["credential", "models", "sign"]);
}

#[test]
fn a_function_the_provider_never_registered_is_credential_failed() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &{
            std::fs::create_dir_all((dir.join("providers/p.lua")).parent().unwrap()).unwrap();
            dir.join("providers/p.lua")
        },
        "fiber.provider(\"p\", { models = { timeout = 1000, run = function() return {} end } })\n",
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        fakes::clock::FakeClock::new(),
    ));
    let provider = LuaProvider::new(Arc::clone(&extension), "p");
    let tokens = Arc::clone(&provider);
    let err = fakes::within("tokens.token", fakes::MUST_SUCCEED_WITHIN, move || {
        tokens.token(&pair(tokens.name(), "default"))
    })
    .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Credential(inner) if matches!(inner.as_ref(), Error::UnknownCallback { .. })
        ),
        "{err:?}"
    );
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    assert!(
        fakes::within("provider.models", fakes::MUST_SUCCEED_WITHIN, move || {
            provider.models(&ModelsArg::default())
        })
        .unwrap()
        .is_empty()
    );
    let none = fakes::within(
        "extension.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || extension.provider_functions("nobody"),
    )
    .unwrap();
    assert!(none.is_empty());
}

#[test]
fn a_provider_function_without_a_timeout_or_an_unknown_one_is_an_error_at_its_line() {
    for (spec, says) in [
        (
            "{ models = function() end }",
            "fiber.provider: `models`: `timeout`",
        ),
        (
            "{ list = { timeout = 1, run = function() end } }",
            "`list` is not models",
        ),
        ("{ sign = { timeout = 1 } }", "`run` must be a function"),
    ] {
        let setup = Setup::new();
        let dir = setup.home().join("ext");
        write(
            &{
                std::fs::create_dir_all((dir.join("providers/p.lua")).parent().unwrap()).unwrap();
                dir.join("providers/p.lua")
            },
            &format!("-- one\nfiber.provider(\"p\", {spec})\n"),
        );
        let extension = Arc::new(LuaExtension::new(
            "ext",
            dir,
            setup.home(),
            fakes::clock::FakeClock::new(),
        ));
        let err = fakes::within(
            "extension.provider_functions",
            fakes::MUST_SUCCEED_WITHIN,
            move || extension.provider_functions("p"),
        )
        .unwrap_err();
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert!(message.starts_with("providers/p.lua:2: "), "{message}");
        assert!(message.contains(says), "{message}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_host_http_call_gives_up_at_the_callbacks_deadline() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, "held")]).unwrap();
    server.hold();
    let url = format!("{}/", server.url());
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"get\", {{ timeout = 30000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"ok\", {{ timeout = 1000, run = function() return \"ok\" end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
    let asked = clock.now();
    let call = Arc::clone(&extension);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(call.command("get", "")));
    assert!(
        server.await_requests(1, WAIT),
        "waited for get to reach the server"
    );
    // The caller waits out the grace. The extension thread fails a parked
    // callback at the deadline, which is only the 30000 ms.
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(30000) + Duration::from_secs(1),
            WAIT
        ),
        "waited for get to park at its grace"
    );
    let scheduler = thread_parked_at(&clock, asked + Duration::from_millis(30000));
    clock.advance_when_parked(
        Some(asked + Duration::from_millis(30000)),
        Some(scheduler),
        Duration::from_millis(30000),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for get")
        .unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 30000);
    // A timeout of one parked callback leaves the extension's thread running.
    let again = Arc::clone(&extension);
    assert_eq!(
        fakes::within("again.command", fakes::MUST_SUCCEED_WITHIN, move || again
            .command("ok", ""))
        .unwrap(),
        "ok"
    );
}

#[track_caller]
fn signed(signer: &Arc<dyn Signer>, headers: &[(String, String)]) -> Vec<(String, String)> {
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let body = br#"{"model":"m"}"#.to_vec();
    let signer = Arc::clone(signer);
    let owned: Vec<(String, String)> = headers.to_vec();
    fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &owned,
            body: &body,
        })
    })
    .unwrap()
}

const TOKEN: &str = "{ token = \"tok-1\", expires_at = 1700003600 }";

#[test]
fn the_token_rides_before_what_sign_returns_and_sign_sees_it() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some(TOKEN),
        Some("{ [\"x-saw-auth\"] = request.headers.authorization or \"missing\" }"),
    );
    assert_eq!(
        fakes::within("provider.functions", fakes::MUST_SUCCEED_WITHIN, {
            let provider = Arc::clone(&provider);
            move || provider.functions()
        })
        .unwrap(),
        ["credential", "sign"]
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    let headers = signed(
        &(signer as Arc<dyn Signer>),
        &[("x-client".to_owned(), "fiber".to_owned())],
    );
    assert_eq!(
        headers,
        [
            ("authorization".to_owned(), "Bearer tok-1".to_owned()),
            ("x-saw-auth".to_owned(), "Bearer tok-1".to_owned()),
        ]
    );
}

#[test]
fn without_sign_only_the_token_is_sent() {
    let setup = Setup::new();
    let provider = script_provider(&setup, FakeClock::new(), Some(TOKEN), None);
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        signed(&(signer as Arc<dyn Signer>), &[]),
        [("authorization".to_owned(), "Bearer tok-1".to_owned())]
    );
}

#[test]
fn without_credential_only_what_sign_returns_is_sent() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        None,
        Some("{ [\"x-s\"] = \"v\" }"),
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        signed(&(signer as Arc<dyn Signer>), &[]),
        [("x-s".to_owned(), "v".to_owned())]
    );
}

#[test]
fn without_credential_or_sign_there_is_no_signer() {
    let setup = Setup::new();
    let provider = script_provider(&setup, FakeClock::new(), None, None);
    assert_eq!(
        fakes::within("provider.functions", fakes::MUST_SUCCEED_WITHIN, {
            let provider = Arc::clone(&provider);
            move || provider.functions()
        })
        .unwrap(),
        Vec::<String>::new()
    );
    assert!(
        fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
            let provider = Arc::clone(&provider);
            move || provider.signer(pair(provider.name(), "default"))
        })
        .unwrap()
        .is_none()
    );
}

#[test]
fn registers_is_true_for_a_registered_function_only() {
    let setup = Setup::new();
    let provider = script_provider(&setup, FakeClock::new(), Some(TOKEN), None);
    let registered = |function: &'static str| {
        fakes::within("provider.registers", fakes::MUST_SUCCEED_WITHIN, {
            let provider = Arc::clone(&provider);
            move || provider.registers(function)
        })
        .unwrap()
    };
    assert!(registered("credential"));
    assert!(!registered("sign"));
    assert!(!registered("models"));

    let setup = Setup::new();
    let bare = script_provider(&setup, FakeClock::new(), None, None);
    assert!(
        !fakes::within("bare.registers", fakes::MUST_SUCCEED_WITHIN, {
            let bare = Arc::clone(&bare);
            move || bare.registers("credential")
        })
        .unwrap()
    );
}

#[test]
fn sign_wins_over_the_token_header_whatever_its_case() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some(TOKEN),
        Some("{ Authorization = \"Bearer custom\" }"),
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        signed(&(signer as Arc<dyn Signer>), &[]),
        [("Authorization".to_owned(), "Bearer custom".to_owned())]
    );
}

#[test]
fn signer_credentials_returns_the_cached_token_even_when_sign_replaces_authorization() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some(TOKEN),
        Some("{ Authorization = \"Bearer custom\" }"),
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    assert!(
        signer.credentials().is_empty(),
        "no sign yet, no cached token"
    );
    assert_eq!(
        signed(&(signer.clone() as Arc<dyn Signer>), &[]),
        [("Authorization".to_owned(), "Bearer custom".to_owned())]
    );
    assert_eq!(
        signer
            .credentials()
            .iter()
            .map(|secret| secret.expose().to_owned())
            .collect::<Vec<_>>(),
        ["tok-1"],
        "the replaced token is still reported"
    );
}

#[test]
fn signer_credentials_is_empty_without_credential() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        None,
        Some("{ [\"x-s\"] = \"v\" }"),
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        signed(&(signer.clone() as Arc<dyn Signer>), &[]),
        [("x-s".to_owned(), "v".to_owned())]
    );
    assert!(signer.credentials().is_empty());
}

#[test]
fn a_credential_error_at_send_time_is_credential_failed() {
    let setup = Setup::new();
    let provider = script_provider(&setup, FakeClock::new(), Some("{}"), Some("{}"));
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let err = fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        })
    })
    .unwrap_err();
    let contract::signing::Error::Credential { code, message, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*code, ErrorCode::CredentialFailed);
    assert_eq!(message, "`ext`: `p.credential` returned no `token`.");
}

/// A provider whose `credential()` refreshes OAuth against `url`.
fn refresh_provider(setup: &Setup, url: &str) -> Arc<LuaProvider> {
    let dir = setup.home().join("ext");
    write(
        &{
            std::fs::create_dir_all((dir.join("providers/p.lua")).parent().unwrap()).unwrap();
            dir.join("providers/p.lua")
        },
        &format!(
            "fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             return host.oauth.refresh(function()\n\
             local reply = host.http({{ url = \"{url}/token\", method = \"POST\" }})\n\
             if reply.status ~= 200 then error(\"refresh failed: \" .. reply.status .. \"\\nsecond line\") end\n\
             return {{ token = \"t\", expires_at = 1700003600 }}\n\
             end) end }},\n\
             sign = {{ timeout = 1000, run = function() return {{}} end }},\n\
             }})\n"
        ),
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        FakeClock::new(),
    ));
    LuaProvider::new(extension, "p")
}

#[track_caller]
fn sign_error(provider: &Arc<LuaProvider>) -> contract::signing::Error {
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        })
    })
    .unwrap_err()
}

#[test]
fn a_rejected_refresh_at_send_time_keeps_authentication_failed() {
    let setup = Setup::new();
    let server = fakes::OauthServer::start(vec![fakes::OauthReply::raw(400, "{}")]);
    let provider = refresh_provider(&setup, &server.url());
    let contract::signing::Error::Credential { code, message, .. } = &sign_error(&provider) else {
        panic!("expected a credential error")
    };
    assert_eq!(*code, ErrorCode::AuthenticationFailed);
    assert!(message.contains("refresh failed: 400"), "{message}");
    assert!(!message.contains("second line"), "{message}");
    assert!(!message.starts_with('`'), "{message}");
}

#[test]
fn an_unreachable_refresh_at_send_time_keeps_connection_failed() {
    let setup = Setup::new();
    let provider = refresh_provider(&setup, &fakes::refused::url());
    let contract::signing::Error::Credential { code, message, .. } = &sign_error(&provider) else {
        panic!("expected a credential error")
    };
    assert_eq!(*code, ErrorCode::ConnectionFailed);
    assert!(!message.starts_with('`'), "{message}");
}

#[test]
fn an_unattended_login_at_send_time_marks_the_credential() {
    let setup = Setup::new();
    // Nobody is attached: the default browser never is, so `host.oauth.open`
    // raises Unattended out of `credential()`.
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some(
            "(function() host.oauth.open(\"https://auth.example/\") return { token = \"t\", \
             expires_at = 1700003600 } end)()",
        ),
        None,
    );
    let contract::signing::Error::Unattended { message } = &sign_error(&provider) else {
        panic!("expected an unattended login")
    };
    assert!(message.contains("host.oauth.open"), "{message}");
    assert!(!message.contains('\n'), "{message}");
}

#[test]
fn declared_provider_files_list_owners_in_order_without_running_lua() {
    let setup = Setup::new();
    let mut manifest = common::manifest("fiber.test/lazy");
    manifest["providers"] = json!({"b":[], "a":[]});
    let source = setup.source("lazy", &manifest, &[]);
    write(&source.join("providers/b.lua"), "error('b must stay lazy')");
    write(&source.join("providers/a.lua"), "error('a must stay lazy')");
    common::install(&setup.home(), &source, "0.1.0").unwrap();
    let configuration = common::config(&setup, &[]);
    let home = setup.home();
    let session = fakes::within(
        "SessionExtensions::load",
        fakes::MUST_SUCCEED_WITHIN,
        move || {
            extensions::SessionExtensions::load(
                &home,
                &configuration,
                FakeClock::new(),
                Arc::new(NoLock),
                None,
            )
        },
    );
    assert!(session.notices().is_empty());
    let names: Vec<_> = session
        .lua_providers()
        .iter()
        .map(|(owner, provider)| (owner.as_str(), provider.name()))
        .collect();
    assert_eq!(names, [("fiber.test/lazy", "a"), ("fiber.test/lazy", "b")]);
}

#[test]
fn a_sign_error_is_its_own_first_line() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some(TOKEN),
        Some("error(\"signing broke\")"),
    );
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let err = fakes::within("provider.sign", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || {
            provider.sign(&SignRequest {
                method: "POST",
                url: &url,
                headers: &[],
                body: b"{}",
            })
        }
    })
    .unwrap_err();
    let contract::signing::Error::Failed(text) = &err else {
        panic!("{err:?}")
    };
    assert!(text.ends_with("signing broke"), "{text}");
    assert!(!text.starts_with('`'), "{text}");
    assert!(!text.contains('\n'), "{text}");
}

#[test]
fn a_credential_error_is_its_own_first_line_with_its_code() {
    let setup = Setup::new();
    let provider = script_provider(
        &setup,
        FakeClock::new(),
        Some("error(\"refresh failed: body-xyz\")"),
        Some("{}"),
    );
    let signer = fakes::within("provider.signer", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.signer(pair(provider.name(), "default"))
    })
    .unwrap()
    .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let err = fakes::within("signer.sign", fakes::MUST_SUCCEED_WITHIN, move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        })
    })
    .unwrap_err();
    let contract::signing::Error::Credential { code, message, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*code, ErrorCode::CredentialFailed);
    assert!(message.ends_with("refresh failed: body-xyz"), "{message}");
    assert!(!message.starts_with('`'), "{message}");
}

#[test]
fn cost_registers_and_is_listed_among_the_provider_functions() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &{
            std::fs::create_dir_all((dir.join("providers/p.lua")).parent().unwrap()).unwrap();
            dir.join("providers/p.lua")
        },
        "fiber.provider(\"p\", { cost = { timeout = 1000, run = function() return 0 end } })\n",
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        FakeClock::new(),
    ));
    let provider = LuaProvider::new(extension, "p");
    let functions = fakes::within("provider.functions", fakes::MUST_SUCCEED_WITHIN, {
        let provider = Arc::clone(&provider);
        move || provider.functions()
    })
    .unwrap();
    assert_eq!(functions, ["cost"]);
    assert!(
        fakes::within(
            "provider.registers",
            fakes::MUST_SUCCEED_WITHIN,
            move || provider.registers("cost")
        )
        .unwrap()
    );
}

#[test]
fn an_unknown_provider_function_is_refused_naming_cost_among_the_known_ones() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &{
            std::fs::create_dir_all((dir.join("providers/p.lua")).parent().unwrap()).unwrap();
            dir.join("providers/p.lua")
        },
        "fiber.provider(\"p\", { list = { timeout = 1, run = function() end } })\n",
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        FakeClock::new(),
    ));
    let err = fakes::within(
        "extension.provider_functions",
        fakes::MUST_SUCCEED_WITHIN,
        move || extension.provider_functions("p"),
    )
    .unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.contains("`list` is not models, quota, credential, sign or cost"),
        "{message}"
    );
}
