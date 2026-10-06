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
use fakes::{ProviderServer, Response, fingerprint};
use serde_json::json;

/// How long a test waits for one call, or for a background refresh.
const WAIT: Duration = Duration::from_secs(5);

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
    assert_eq!(
        requests[0].header("authorization"),
        Some(fingerprint("Bearer k1").as_str())
    );
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
        // Far past the advanced clock, so `t2` is not itself due: a poll that
        // sees it starts no further refresh, and the count stays at two.
        token("t2", Duration::from_secs(2 * 3600)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = fixture_on(&setup, &server, clock.clone());
    let first = Arc::clone(&provider);
    assert_eq!(within(move || first.token()).unwrap().expose(), "t1");
    assert_eq!(server.requests().len(), 1);
    // Into the refresh window, still short of expiry.
    clock.advance(
        Duration::from_secs(3600)
            .checked_sub(REFRESH_BEFORE)
            .unwrap(),
    );
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
        let err = within(move || provider.token()).unwrap_err();
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
    let err = within(move || provider.token()).unwrap_err();
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
        match hold_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
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
fn a_function_the_provider_never_registered_is_credential_failed() {
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
    assert!(
        matches!(
            &err,
            Error::Credential(inner) if matches!(inner.as_ref(), Error::UnknownCallback { .. })
        ),
        "{err:?}"
    );
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
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

/// Accepts one connection, reads its head, reports, and holds the socket
/// for at most 5 seconds without answering.
fn hold_after_head(listener: std::net::TcpListener, accepted: mpsc::Sender<()>) {
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
    if accepted.send(()).is_err() {
        return;
    }
    let (_tx, rx) = mpsc::channel::<()>();
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
    }
    drop(sock);
}

#[test]
fn a_host_http_call_gives_up_at_the_callbacks_deadline() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    std::thread::spawn(move || hold_after_head(listener, accepted_tx));
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
    accepted_rx
        .recv_timeout(WAIT)
        .expect("waited for get to reach the server");
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
    let err = rx.recv_timeout(WAIT).expect("waited for get").unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 200);
    // A timeout of one parked callback leaves the extension's thread running.
    let again = Arc::clone(&extension);
    assert_eq!(within(move || again.command("ok", "")).unwrap(), "ok");
}

/// A test-local provider `p`: `credential` and `sign` run `credential_run`
/// and `sign_run`, each absent when its option is `None`.
fn script_provider(
    setup: &Setup,
    credential_run: Option<&str>,
    sign_run: Option<&str>,
) -> Arc<LuaProvider> {
    let mut spec = Vec::new();
    if let Some(run) = credential_run {
        spec.push(format!(
            "credential = {{ timeout = 5000, run = function() return {run} end }}"
        ));
    }
    if let Some(run) = sign_run {
        spec.push(format!(
            "sign = {{ timeout = 1000, run = function(request) return {run} end }}"
        ));
    }
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!("fiber.provider(\"p\", {{ {} }})\n", spec.join(", ")),
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        FakeClock::new(),
    ));
    LuaProvider::new(extension, "p")
}

fn signed(signer: &Arc<dyn Signer>, headers: &[(String, String)]) -> Vec<(String, String)> {
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let body = br#"{"model":"m"}"#.to_vec();
    let signer = Arc::clone(signer);
    let owned: Vec<(String, String)> = headers.to_vec();
    within(move || {
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
        Some(TOKEN),
        Some("{ [\"x-saw-auth\"] = request.headers.authorization or \"missing\" }"),
    );
    assert_eq!(
        within({
            let provider = Arc::clone(&provider);
            move || provider.functions()
        })
        .unwrap(),
        ["credential", "sign"]
    );
    let signer = within({
        let provider = Arc::clone(&provider);
        move || provider.signer()
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
    let provider = script_provider(&setup, Some(TOKEN), None);
    let signer = within({
        let provider = Arc::clone(&provider);
        move || provider.signer()
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
    let provider = script_provider(&setup, None, Some("{ [\"x-s\"] = \"v\" }"));
    let signer = within({
        let provider = Arc::clone(&provider);
        move || provider.signer()
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
    let provider = script_provider(&setup, None, None);
    assert_eq!(
        within({
            let provider = Arc::clone(&provider);
            move || provider.functions()
        })
        .unwrap(),
        Vec::<String>::new()
    );
    assert!(
        within({
            let provider = Arc::clone(&provider);
            move || provider.signer()
        })
        .unwrap()
        .is_none()
    );
}

#[test]
fn registers_is_true_for_a_registered_function_only() {
    let setup = Setup::new();
    let provider = script_provider(&setup, Some(TOKEN), None);
    let registered = |function: &'static str| {
        within({
            let provider = Arc::clone(&provider);
            move || provider.registers(function)
        })
        .unwrap()
    };
    assert!(registered("credential"));
    assert!(!registered("sign"));
    assert!(!registered("models"));

    let setup = Setup::new();
    let bare = script_provider(&setup, None, None);
    assert!(
        !within({
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
        Some(TOKEN),
        Some("{ Authorization = \"Bearer custom\" }"),
    );
    let signer = within({
        let provider = Arc::clone(&provider);
        move || provider.signer()
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        signed(&(signer as Arc<dyn Signer>), &[]),
        [("Authorization".to_owned(), "Bearer custom".to_owned())]
    );
}

#[test]
fn a_credential_error_at_send_time_is_credential_failed() {
    let setup = Setup::new();
    let provider = script_provider(&setup, Some("{}"), Some("{}"));
    let signer = within({
        let provider = Arc::clone(&provider);
        move || provider.signer()
    })
    .unwrap()
    .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    let err = within(move || {
        signer.sign(&SignRequest {
            method: "POST",
            url: &url,
            headers: &[],
            body: b"{}",
        })
    })
    .unwrap_err();
    let contract::signing::Error::Credential { code, message } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*code, ErrorCode::CredentialFailed);
    assert_eq!(message, "`ext`: `p.credential` returned no `token`.");
}

/// A provider whose `credential()` refreshes OAuth against `url`.
fn refresh_provider(setup: &Setup, url: &str) -> Arc<LuaProvider> {
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.provider(\"p\", {{\n\
             credential = {{ timeout = 60000, run = function()\n\
             return host.oauth.refresh(function()\n\
             local reply = host.http({{ url = \"{url}/token\", method = \"POST\" }})\n\
             if reply.status ~= 200 then error(\"refresh failed: \" .. reply.status) end\n\
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

fn sign_error(provider: &Arc<LuaProvider>) -> contract::signing::Error {
    let signer = within({
        let provider = Arc::clone(provider);
        move || provider.signer()
    })
    .unwrap()
    .unwrap();
    let url = "http://127.0.0.1:1/v1/responses".to_owned();
    within(move || {
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
    let contract::signing::Error::Credential { code, message } = &sign_error(&provider) else {
        panic!("expected a credential error")
    };
    assert_eq!(*code, ErrorCode::AuthenticationFailed);
    assert!(message.contains("refresh failed: 400"), "{message}");
}

#[test]
fn an_unreachable_refresh_at_send_time_keeps_connection_failed() {
    let setup = Setup::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let provider = refresh_provider(&setup, &format!("http://127.0.0.1:{port}"));
    let contract::signing::Error::Credential { code, .. } = &sign_error(&provider) else {
        panic!("expected a credential error")
    };
    assert_eq!(*code, ErrorCode::ConnectionFailed);
}

#[test]
fn provider_names_lists_every_registered_provider_sorted() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        "fiber.provider(\"b\", { models = { timeout = 1000, run = function() return {} end } })\n\
         fiber.provider(\"a\", { sign = { timeout = 1000, run = function() return {} end } })\n",
    );
    let extension = Arc::new(LuaExtension::new(
        "ext",
        dir,
        setup.home(),
        FakeClock::new(),
    ));
    let names = within(move || extension.provider_names()).unwrap();
    assert_eq!(names, ["a", "b"]);
}
