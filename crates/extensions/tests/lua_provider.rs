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

use std::io::Read;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

use common::{Setup, write};
use config::{Secret, store_secret};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::signing::{SignRequest, Signer};
use extensions::{Error, LuaExtension, LuaProvider, REFRESH_BEFORE};
use fakes::clock::FakeClock;
use fakes::{ProviderServer, Response};
use serde_json::json;

/// How long a test waits for one call, or for a background refresh.
const WAIT: Duration = Duration::from_secs(10);

/// Runs `f` on its own thread under `WAIT`, so a call that never returns
/// fails the test instead of hanging it.
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
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
    let extension = Arc::new(LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        &home,
        clock,
    ));
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
    let models = within({
        let provider = Arc::clone(&provider);
        move || provider.models()
    })
    .unwrap();
    assert_eq!(ids(&models), ["m1", "m2"]);
    assert_eq!(models[0].base_url, format!("{}/v1", server.url()));
    assert_eq!(models[0].context_window, Some(1000));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/models");
    assert_eq!(requests[0].header("authorization"), Some("<masked>"));
    let cached = config::read_model_cache(&setup.home(), "fixture").unwrap();
    assert_eq!(cached.as_deref(), Some(models.as_slice()));

    // Asked again, it serves what it has.
    assert_eq!(ids(&provider.models().unwrap()), ["m1", "m2"]);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_cached_list_is_served_without_running_lua_until_the_refresh_returns() {
    let setup = Setup::new();
    let server = ProviderServer::start([listing(&["new"])]).unwrap();
    let provider = fixture(&setup, &server);
    let old = json!([{ "id": "old", "protocol": "openai-responses", "base_url": "http://x/v1" }]);
    write(
        &setup.home().join("cache/models/fixture.json"),
        &old.to_string(),
    );

    assert_eq!(ids(&provider.models().unwrap()), ["old"]);
    assert!(server.requests().is_empty(), "the cached copy is served");

    let refresh = provider.refresh_models();
    let refreshed = within(move || refresh.join().unwrap()).unwrap();
    assert_eq!(ids(&refreshed), ["new"]);
    assert_eq!(ids(&provider.models().unwrap()), ["new"]);
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
        let err = within(move || provider.models()).unwrap_err();
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
        assert_eq!(within(move || provider.token()).unwrap().expose(), "t1");
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
        token("t2", Duration::from_secs(3600)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = fixture_on(&setup, &server, clock.clone());
    let first = Arc::clone(&provider);
    assert_eq!(within(move || first.token()).unwrap().expose(), "t1");
    assert_eq!(server.requests().len(), 1);
    // Into the refresh window, still short of expiry.
    clock.advance(Duration::from_secs(3600) - REFRESH_BEFORE);
    let second = Arc::clone(&provider);
    assert_eq!(within(move || second.token()).unwrap().expose(), "t1");
    assert!(
        server.await_requests(2, WAIT),
        "waited for the refresh request"
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match provider.token() {
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
    rx.recv_timeout(WAIT)
        .expect("waited for the refreshed token");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn an_expired_token_just_returned_is_an_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([token("t1", Duration::ZERO)]).unwrap();
    let provider = fixture(&setup, &server);
    let err = within(move || provider.token()).unwrap_err();
    assert!(matches!(err, Error::BadReturn { .. }), "{err:?}");
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
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
        let err = within(move || provider.token()).unwrap_err();
        assert!(matches!(err, Error::BadReturn { .. }), "{body}: {err:?}");
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        assert!(err.to_string().contains("expires_at"), "{err}");
    }
}

#[test]
fn a_credential_with_no_token_is_extension_failed() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, r#"{"expires_at": 1}"#)]).unwrap();
    let provider = fixture(&setup, &server);
    let err = within(move || provider.token()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    let Error::BadReturn {
        extension,
        callback,
        ..
    } = &err
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
    let mut script = String::new();
    for (name, returns) in [
        ("empty", "{}"),
        ("none", "nil"),
        ("list", "{ \"a\" }"),
        ("number", "1"),
    ] {
        script.push_str(&format!(
            "fiber.provider(\"{name}\", {{ sign = {{ timeout = 1000, run = function() return {returns} end }} }})\n"
        ));
    }
    write(&dir.join("init.lua"), &script);
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
        let signed = within(move || {
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

#[test]
fn sign_sees_the_bodys_hash_never_the_body_and_adds_headers() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    let provider = fixture(&setup, &server);
    let body = br#"{"model":"m1"}"#;
    let url = format!("{}/v1/responses", server.url());
    let signer = Arc::clone(&provider);
    let call_url = url.clone();
    let headers = within(move || {
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
    assert_eq!(get("x-fixture-saw"), "body_sha256,headers,method,url");
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0; 1];
        let mut seen = Vec::new();
        loop {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => seen.push(buf[0]),
            }
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        if accepted_tx.send(()).is_err() {
            return;
        }
        let (_hold_tx, hold_rx) = mpsc::channel::<()>();
        match hold_rx.recv() {
            Ok(()) | Err(mpsc::RecvError) => {}
        }
    });
    let home = setup.home();
    store_secret(&home, "fixture.url", &Secret::new(url)).unwrap();
    store_secret(&home, "fixture.api_key", &Secret::new("k1".into())).unwrap();
    let extension = Arc::new(LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        &home,
        fakes::clock::FakeClock::new(),
    ));
    let provider = LuaProvider::new(extension, "fixture");
    let refresh = provider.refresh_models();
    accepted_rx
        .recv_timeout(WAIT)
        .expect("waited for models() to reach the server");
    let signer = Arc::clone(&provider);
    let headers = within(move || {
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
    let functions = within(move || extension.provider_functions("fixture")).unwrap();
    assert_eq!(functions, ["credential", "models", "sign"]);
}

#[test]
fn a_function_the_provider_never_registered_is_extension_failed() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
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
    let err = within(move || tokens.token()).unwrap_err();
    assert!(matches!(err, Error::UnknownCallback { .. }), "{err:?}");
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    assert!(within(move || provider.models()).unwrap().is_empty());
    let none = within(move || extension.provider_functions("nobody")).unwrap();
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
            &dir.join("init.lua"),
            &format!("-- one\nfiber.provider(\"p\", {spec})\n"),
        );
        let extension = Arc::new(LuaExtension::new(
            "ext",
            dir,
            setup.home(),
            fakes::clock::FakeClock::new(),
        ));
        let err = within(move || extension.provider_functions("p")).unwrap_err();
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert!(message.starts_with("init.lua:2: "), "{message}");
        assert!(message.contains(says), "{message}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_host_http_call_gives_up_at_the_callbacks_deadline() {
    let setup = Setup::new();
    // Accepts connections into its backlog and never answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", silent.local_addr().unwrap());
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"get\", {{ timeout = 200, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"ok\", {{ timeout = 1000, run = function() return \"ok\" end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let extension = Arc::new(LuaExtension::new("ext", dir, setup.home(), clock.clone()));
    let asked = clock.now();
    let call = Arc::clone(&extension);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(call.command("get", "")));
    // The caller waits out the grace. The extension thread fails a parked
    // callback at the deadline, which is only the 200 ms.
    assert!(
        clock.await_parked(
            asked + Duration::from_millis(200) + Duration::from_secs(1),
            WAIT
        ),
        "waited for get to park at its grace"
    );
    clock.advance(Duration::from_millis(200));
    let err = rx
        .recv_timeout(WAIT)
        .expect("waited for get")
        .unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 200);
    // A timeout of one parked callback leaves the extension's thread running.
    let again = Arc::clone(&extension);
    assert_eq!(within(move || again.command("ok", "")).unwrap(), "ok");
}
