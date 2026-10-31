//! `login`: the error codes, the redaction, discovery and registration,
//! and the authorization, callback and exchange against a routed fake
//! server and a fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::callback::{MAX_HEAD, SILENT_POLLS, Served, answer, decode};
use super::*;
use crate::test_support::{FakeTokens, WITHIN};
use contract::diag::{DebugLog, HttpRequest, Purpose};
use contract::mcp_tokens::TokenFiles;
use fakes::{Deadline, ProviderServer, Request, Response};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

#[test]
fn string_bearing_variants_never_print_their_string() {
    for error in [
        LoginError::Unreachable("secret".to_owned()),
        LoginError::Discovery("secret".to_owned()),
        LoginError::Registration("secret".to_owned()),
        LoginError::Exchange("secret".to_owned()),
        LoginError::Callback("secret".to_owned()),
        LoginError::Denied("secret".to_owned()),
    ] {
        let debug = format!("{error:?}");
        assert!(!debug.contains("secret"), "debug leaks: {debug}");
    }
}

#[test]
fn each_login_error_maps_to_its_stable_code() {
    use crate::token::TokenError;
    use contract::mcp_tokens::TokenFileError;
    for (error, code) in [
        (LoginError::Url, ErrorCode::ConfigInvalid),
        (LoginError::Userinfo, ErrorCode::ConfigInvalid),
        (
            LoginError::Unreachable("step".to_owned()),
            ErrorCode::ConnectionFailed,
        ),
        (
            LoginError::Discovery("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Registration("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Exchange("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Callback("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Denied("access_denied".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (LoginError::PkceUnsupported, ErrorCode::ConfigInvalid),
        (LoginError::Random, ErrorCode::AuthenticationFailed),
        (LoginError::Timeout, ErrorCode::AuthenticationFailed),
        (
            LoginError::Store(TokenError::Url),
            ErrorCode::InvalidArguments,
        ),
        (LoginError::Store(TokenError::Busy), ErrorCode::IoFailed),
        (
            LoginError::Store(TokenError::Files(TokenFileError::Unsynced("path".into()))),
            ErrorCode::IoFailed,
        ),
    ] {
        assert_eq!(error.code(), code, "{error:?}");
    }
}

#[test]
fn pkce_unsupported_names_no_code() {
    let error = LoginError::PkceUnsupported;
    assert_eq!(
        error.to_string(),
        "its authorization server does not offer PKCE with S256"
    );
}

/// The fake authorization server's base, from the request's host.
fn base_of(request: &Request) -> String {
    format!("http://{}", request.header("host").unwrap_or_default())
}

fn json_response(value: &Value) -> Response {
    Response::status(200, value.to_string())
}

fn authorization_document(base: &str) -> Value {
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    })
}

fn token_reply(access: &str) -> Value {
    json!({
        "access_token": access,
        "refresh_token": "rt-1",
        "token_type": "Bearer",
        "expires_in": 3600,
    })
}

/// A routed fake serving the whole flow on one server: the probe at
/// `/mcp`, the documents, `/register` and `/token`. The documents name
/// the server's own base, read from each request's host.
fn start_full() -> ProviderServer {
    ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}").header(
                "www-authenticate",
                &format!("Bearer resource_metadata=\"{base}/prm\""),
            ),
            ("GET", "/prm") => json_response(&json!({"authorization_servers": [base.clone()]})),
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("GET", "/.well-known/openid-configuration") => Response::status(404, "{}"),
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap()
}

fn server_url(server: &ProviderServer) -> String {
    format!("{}/mcp", server.url())
}

/// The value of `name` in `url`'s query, percent-decoded.
fn query_param(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| decode(value))
    })
}

/// Sends one callback GET to `redirect_uri` with `query`, and reads the
/// reply to the close.
fn send_callback(redirect_uri: &str, query: &str) -> String {
    let (_, rest) = redirect_uri.split_once("://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .write_all(
            format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

/// Runs `drive` while a login waits on `clock`, advancing the clock past
/// the callback deadline when `drive` panics so the parked login still
/// ends and the scope still joins; the panic is then resumed. Without the
/// release a value mutant that breaks the flow before the callback would
/// hang the suite past its timeout instead of failing the test.
fn drive_releasing_on_panic<T>(
    clock: &Arc<fakes::clock::FakeClock>,
    drive: impl FnOnce() -> T,
) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(drive)) {
        Ok(value) => value,
        Err(payload) => {
            // Failure-path release inside a panic handler that re-raises the original payload; it proves nothing (ruling 6203478697).
            #[allow(
                clippy::disallowed_methods,
                reason = "panic-handler release that re-raises the original payload (ruling 6203478697)"
            )]
            clock.advance(CALLBACK_WAIT + Duration::from_secs(1));
            std::panic::resume_unwind(payload);
        }
    }
}

/// Runs `login` to its result, driving one callback with `query_for` built
/// from the authorization URL. `query_for` receives the URL `present` saw.
/// The URL wait runs on a scoped driver thread, and the test waits only on
/// the login's own result: a login that fails before presenting, such as a
/// refused discovery, ends the flow with no URL, and the driver leaves with
/// the hung-up channel while the result names why, instead of the test
/// sitting out a URL wait on the real clock.
#[track_caller]
fn run_with_callback(
    server_url: &str,
    files: &FakeTokens,
    clock: &Arc<fakes::clock::FakeClock>,
    query_for: impl Fn(&str) -> String + Send,
) -> Result<(), LoginError> {
    let (sent, received) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    let clock = Arc::clone(clock);
    std::thread::scope(|scope| {
        // `sent` moves into the login thread: when the login fails before
        // presenting, its end drops the URL sender, which releases the
        // driver's blocking receive below. Owned outside, the sender would
        // outlive the scope and the driver would never leave.
        let login = scope.spawn({
            let clock = Arc::clone(&clock);
            move || {
                let result = login(
                    files,
                    server_url,
                    &|url| sent.send(url.to_owned()).unwrap(),
                    &*clock,
                );
                sent_done.send(result).unwrap();
            }
        });
        let _driver = scope.spawn({
            let clock = Arc::clone(&clock);
            move || {
                // No URL ever comes when the login fails before the wait:
                // the hung-up channel ends the driver, and the result below
                // names the failure.
                let Ok(url) = received.recv() else {
                    return;
                };
                drive_releasing_on_panic(&clock, || {
                    let query = query_for(&url);
                    let redirect = query_param(&url, "redirect_uri").unwrap().to_owned();
                    send_callback(&redirect, &query);
                });
            }
        });
        // The result first, through the deadline: joining a stuck login
        // would hang past the hang guard. On expiry the clock wait is
        // released so the scope can still join.
        let result = match fakes::Deadline::after(WITHIN).recv(&received_done) {
            Ok(result) => result,
            Err(_) => {
                clock.advance_when_parked(
                    Some(clock.origin() + CALLBACK_WAIT),
                    None,
                    CALLBACK_WAIT,
                    &fakes::Deadline::after(WITHIN),
                );
                login.join().unwrap();
                panic!("waited until the deadline for the driven login");
            }
        };
        login.join().unwrap();
        result
    })
}

/// Drives the success callback: `code=abc` with the URL's own `state`.
fn succeed(url: &str) -> String {
    let state = query_param(url, "state").unwrap();
    format!("code=abc&state={state}")
}

#[test]
fn a_url_with_userinfo_is_refused_before_any_request() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &format!("{}/mcp", server.url()).replacen("://", "://user@", 1),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Userinfo));
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn a_non_http_url_is_refused_before_any_request() {
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        "ws://example.test/mcp",
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Url));
}

#[test]
fn a_dead_server_is_unreachable_and_stores_nothing() {
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        "http://127.0.0.1:1/mcp",
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Unreachable(_)));
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
    assert!(files.read("anything").unwrap().is_none());
}

#[test]
fn a_404_continues_while_other_statuses_fail_discovery() {
    // The protected-resource root answers per `status`; the
    // path-inserted candidate 404s first.
    for (status, fails) in [(404, false), (403, true), (500, true), (302, true)] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => Response::status(404, "{}"),
                ("GET", "/.well-known/oauth-protected-resource") => {
                    if status == 404 {
                        json_response(&json!({"authorization_servers": [base.clone()]}))
                    } else {
                        Response::status(status, "secret-body")
                    }
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&authorization_document(&base))
                }
                ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
                ("POST", "/token") => json_response(&token_reply("at-1")),
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        if fails {
            // Before any callback: the failure is terminal at discovery.
            let error = login(
                &files,
                &url,
                &|_| panic!("no authorization URL is shown"),
                &*clock,
            )
            .unwrap_err();
            assert!(
                matches!(error, LoginError::Discovery(_)),
                "{status}: {error:?}"
            );
            assert!(
                !error.to_string().contains("secret-body"),
                "{status}: {}",
                error
            );
            assert!(!format!("{error:?}").contains("secret-body"));
        } else {
            run_with_callback(&url, &files, &clock, succeed).unwrap();
            assert!(
                files
                    .read(&crate::token::token_key(&url).unwrap())
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[test]
fn a_dropped_metadata_connection_fails_discovery() {
    let server = ProviderServer::start_responding(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => Response::drop_connection(),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
}

#[test]
fn a_non_json_metadata_document_fails_discovery() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                Response::status(200, "not json secret")
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
    assert!(!error.to_string().contains("not json secret"));
}

#[test]
fn an_absent_authorization_server_fails_discovery_naming_the_step() {
    let server = ProviderServer::start_responding(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
    assert!(
        error.to_string().contains("authorization-server metadata"),
        "{}",
        error
    );
}

#[test]
fn a_body_past_one_mib_fails_while_exactly_one_mib_reads() {
    for size in [MAX_BODY, MAX_BODY + 1] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    let mut body = json!({"authorization_servers": [base.clone()]}).to_string();
                    body.extend(std::iter::repeat_n(' ', size.saturating_sub(body.len())));
                    Response::status(200, body)
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&authorization_document(&base))
                }
                ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
                ("POST", "/token") => json_response(&token_reply("at-1")),
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        if size > MAX_BODY {
            let error = login(
                &files,
                &url,
                &|_| panic!("no authorization URL is shown"),
                &*clock,
            )
            .unwrap_err();
            assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
            assert!(
                files
                    .read(&crate::token::token_key(&url).unwrap())
                    .unwrap()
                    .is_none()
            );
        } else {
            run_with_callback(&url, &files, &clock, succeed).unwrap();
        }
    }
}

#[test]
fn registration_failures_are_terminal_with_no_second_request() {
    // A non-2xx, a dropped connection, and a malformed body each fail
    // registration after exactly one request; the token endpoint sees none.
    for case in ["status", "dropped", "malformed"] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    json_response(&json!({"authorization_servers": [base.clone()]}))
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&authorization_document(&base))
                }
                ("POST", "/register") => match case {
                    "status" => Response::status(500, "secret-body"),
                    "dropped" => Response::drop_connection(),
                    _ => Response::status(200, "not json"),
                },
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        // Before any callback: a failed registration never reaches it.
        let error = login(
            &files,
            &server_url(&server),
            &|_| panic!("no authorization URL is shown"),
            &*clock,
        )
        .unwrap_err();
        assert!(
            matches!(error, LoginError::Registration(_)),
            "{case}: {error:?}"
        );
        assert!(
            !error.to_string().contains("secret-body"),
            "{case}: {error}"
        );
        let requests = server.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path == "/register")
                .count(),
            1,
            "{case}: a failed registration retried"
        );
        assert!(
            requests.iter().all(|request| request.path != "/token"),
            "{case}: the exchange ran after a failed registration"
        );
    }
}

#[test]
fn a_missing_registration_endpoint_fails_naming_it() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_response(&json!({
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "code_challenge_methods_supported": ["S256"],
            })),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
    assert!(error.to_string().contains("client registration"), "{error}");
}

#[test]
fn a_token_endpoint_with_userinfo_fails_and_stores_nothing() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_response(&json!({
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": "https://u:p@h/token",
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"],
            })),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = login(
        &files,
        &url,
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_registration_body_carries_the_public_client_shape() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    run_with_callback(&server_url(&server), &files, &clock, succeed).unwrap();
    let register = server
        .requests()
        .into_iter()
        .find(|request| request.path == "/register")
        .unwrap();
    let body: Value = serde_json::from_slice(&register.body).unwrap();
    assert_eq!(body["grant_types"], json!(["authorization_code"]));
    assert_eq!(body["response_types"], json!(["code"]));
    assert_eq!(body["token_endpoint_auth_method"], json!("none"));
    assert!(body["client_name"].is_string());
    assert_eq!(body["redirect_uris"].as_array().unwrap().len(), 1);
    assert!(
        body["redirect_uris"][0]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
}

#[test]
fn the_header_document_is_fetched_and_the_first_server_is_used() {
    let second = ProviderServer::start([Response::status(500, "{}")]).unwrap();
    let second_base = second.url();
    let server = ProviderServer::start_responding(move |request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}").header(
                "www-authenticate",
                &format!("Bearer resource_metadata=\"{base}/prm\""),
            ),
            ("GET", "/prm") => json_response(
                &json!({"authorization_servers": [base.clone(), second_base.clone()]}),
            ),
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    assert_eq!(second.requests().len(), 0, "the second server was reached");
    // The header's document is fetched: a mutant that ignores the header
    // never requests it.
    let paths: Vec<String> = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(
        paths.contains(&"/prm".to_owned()),
        "the header's document was not fetched: {paths:?}"
    );
    let stored = files
        .read(&crate::token::token_key(&url).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored["token_endpoint"],
        json!(format!("{}/token", server.url()))
    );
}

#[test]
fn without_protected_resource_metadata_the_origin_is_its_own_server() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(200, "{}"),
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    // A pathless URL fetches the root document once: a mutant that widens
    // the path-inserted guard fetches it twice.
    let roots = server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/.well-known/oauth-protected-resource")
        .count();
    assert_eq!(roots, 1, "the root document was fetched {roots} times");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_root_path_fetches_only_the_root_protected_resource_document() {
    // A canonical path of `/` holds exactly one side of the
    // path-inserted guard (`!path.is_empty()` but not `path != "/"`),
    // so only the root document is fetched: an `||` mutant also fetches
    // the slash-suffixed inserted document, which answers 500 here and
    // fails discovery before anything is presented, so the flow's own
    // result fails the `unwrap` below with no URL wait, and the exact
    // request list beneath catches what it fetched.
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Response::status(200, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/") => {
                Response::status(500, "secret-body")
            }
            ("GET", "/.well-known/oauth-protected-resource") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = format!("{}/", server.url());
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let paths: Vec<String> = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(
        paths
            .iter()
            .filter(|path| *path == "/.well-known/oauth-protected-resource")
            .count(),
        1,
        "the root document fetches: {paths:?}"
    );
    assert!(
        paths
            .iter()
            .all(|path| path != "/.well-known/oauth-protected-resource/"),
        "the slash-suffixed inserted document was fetched: {paths:?}"
    );
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_well_known_paths_are_tried_path_inserted_first() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => Response::status(404, "{}"),
            ("GET", "/.well-known/oauth-protected-resource") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => Response::status(404, "{}"),
            ("GET", "/.well-known/openid-configuration") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let paths: Vec<String> = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(paths.contains(&"/.well-known/oauth-protected-resource/mcp".to_owned()));
    assert!(paths.contains(&"/.well-known/openid-configuration".to_owned()));
    // The pathless base names its first two candidates alike, and the
    // duplicate is fetched once before falling through to `openid-configuration`.
    assert_eq!(
        paths
            .iter()
            .filter(|path| *path == "/.well-known/oauth-authorization-server")
            .count(),
        1
    );
}

/// Starts a fake whose authorization server is the path-bearing
/// `{base}/tenant`, serving its authorization-server document at
/// `document_at` while every other well-known URL 404s. The document
/// names the tenant issuer, so registration and the exchange run under
/// `/tenant`.
fn start_tenant_issuer(document_at: &'static str) -> ProviderServer {
    ProviderServer::start_responding(move |request: &Request| {
        let base = base_of(request);
        let issuer = format!("{base}/tenant");
        if request.method == "GET" && request.path == document_at {
            return json_response(&authorization_document(&issuer));
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [issuer]}))
            }
            ("POST", "/tenant/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/tenant/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap()
}

/// The authorization-server metadata paths the fake saw, in order,
/// skipping the protected-resource fetches that always come first.
fn metadata_paths(server: &ProviderServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .filter(|path| {
            path.contains("oauth-authorization-server") || path.contains("openid-configuration")
        })
        .collect()
}

#[test]
fn a_path_bearing_issuer_is_discovered_at_its_rfc_8414_url() {
    // Each row serves the document at one candidate and names the
    // metadata requests the login must have made, in order: the RFC 8414
    // path-inserted URL first, then the base-appended one, then
    // `openid-configuration`.
    for (document_at, expected) in [
        (
            "/.well-known/oauth-authorization-server/tenant",
            vec!["/.well-known/oauth-authorization-server/tenant"],
        ),
        (
            "/tenant/.well-known/oauth-authorization-server",
            vec![
                "/.well-known/oauth-authorization-server/tenant",
                "/tenant/.well-known/oauth-authorization-server",
            ],
        ),
        (
            "/tenant/.well-known/openid-configuration",
            vec![
                "/.well-known/oauth-authorization-server/tenant",
                "/tenant/.well-known/oauth-authorization-server",
                "/tenant/.well-known/openid-configuration",
            ],
        ),
    ] {
        let server = start_tenant_issuer(document_at);
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        run_with_callback(&url, &files, &clock, succeed).unwrap();
        let expected: Vec<String> = expected.into_iter().map(|path| path.to_owned()).collect();
        assert_eq!(metadata_paths(&server), expected, "{document_at}");
        assert!(
            files
                .read(&crate::token::token_key(&url).unwrap())
                .unwrap()
                .is_some(),
            "{document_at}"
        );
    }
}

#[test]
fn a_query_bearing_issuer_with_a_root_path_skips_the_inserted_candidate() {
    // An issuer such as `https://auth.test/?q=1` keeps its slash before
    // the query after trimming, so its path after the query is stripped
    // is exactly `/`: no path-inserted candidate is fetched. A `||`
    // mutant of the guard adds the root inserted document, which this
    // exact request list catches.
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        let issuer = format!("{base}/?q=1");
        if request.method == "GET" && request.path == "/?q=1/.well-known/oauth-authorization-server"
        {
            return json_response(&json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"],
            }));
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [issuer]}))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    assert_eq!(
        metadata_paths(&server),
        vec!["/?q=1/.well-known/oauth-authorization-server".to_owned()],
    );
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn without_s256_the_login_is_refused_before_any_registration() {
    for document in [
        json!({
            "authorization_endpoint": "http://x/authorize",
            "token_endpoint": "http://x/token",
            "registration_endpoint": "http://x/register",
            "code_challenge_methods_supported": ["plain"],
        }),
        json!({
            "authorization_endpoint": "http://x/authorize",
            "token_endpoint": "http://x/token",
            "registration_endpoint": "http://x/register",
        }),
    ] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    json_response(&json!({"authorization_servers": [base.clone()]}))
                }
                ("GET", "/.well-known/oauth-authorization-server") => json_response(&document),
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        let error = login(
            &files,
            &url,
            &|_| panic!("no authorization URL is shown"),
            &*clock,
        )
        .unwrap_err();
        assert!(matches!(error, LoginError::PkceUnsupported), "{error:?}");
        assert_eq!(error.code(), ErrorCode::ConfigInvalid);
        assert!(
            server
                .requests()
                .iter()
                .all(|request| request.path != "/register"),
            "a registration request was sent"
        );
        assert!(
            files
                .read(&crate::token::token_key(&url).unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_failing_random_source_fails_closed_with_no_side_effects() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = super::login_with_random(
        &files,
        &url,
        &|_| panic!("no authorization URL is shown"),
        &*clock,
        None,
        &|_| Err(ring::error::Unspecified),
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Random), "{error:?}");
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    assert_eq!(
        error.to_string(),
        "Fiber could not generate the login's PKCE secrets."
    );
    assert_eq!(server.requests().len(), 0, "the login reached the server");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_authorization_url_and_exchange_carry_the_pkce_shape() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let canonical = crate::token::canonical_url(&url).unwrap();
    run_with_callback(&url, &files, &clock, |shown| {
        let redirect = query_param(shown, "redirect_uri").unwrap();
        assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
        assert!(redirect.ends_with("/callback"), "{redirect}");
        assert_eq!(query_param(shown, "response_type").as_deref(), Some("code"));
        assert_eq!(query_param(shown, "client_id").as_deref(), Some("c1"));
        assert_eq!(
            query_param(shown, "code_challenge_method").as_deref(),
            Some("S256")
        );
        assert_eq!(
            query_param(shown, "resource").as_deref(),
            Some(canonical.as_str())
        );
        let challenge = query_param(shown, "code_challenge").unwrap();
        let state = query_param(shown, "state").unwrap();
        assert_eq!(challenge.len(), 43, "{challenge}");
        assert_eq!(state.len(), 43, "{state}");
        succeed(shown)
    })
    .unwrap();
    let exchange = server
        .requests()
        .into_iter()
        .find(|request| request.path == "/token")
        .unwrap();
    let form: Vec<(String, String)> = exchange
        .body
        .split(|byte| *byte == b'&')
        .map(|pair| {
            let text = String::from_utf8_lossy(pair).into_owned();
            let (key, value) = text.split_once('=').unwrap();
            (decode(key), decode(value))
        })
        .collect();
    let field = |name| form.iter().find(|(key, _)| key == name).unwrap().1.clone();
    assert_eq!(field("grant_type"), "authorization_code");
    assert_eq!(field("code"), "abc");
    assert_eq!(field("client_id"), "c1");
    assert_eq!(field("resource"), canonical);
    assert_eq!(field("code_verifier").len(), 43);
    let redirect = field("redirect_uri");
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    assert_eq!(form.len(), 6);
}

#[test]
fn an_authorization_endpoint_with_a_query_keeps_it() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_response(&json!({
                "authorization_endpoint": format!("{base}/authorize?tenant=x"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"],
            })),
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, |shown| {
        // The endpoint's own query survives, and every parameter joins it.
        assert_eq!(query_param(shown, "tenant").as_deref(), Some("x"));
        assert_eq!(query_param(shown, "response_type").as_deref(), Some("code"));
        assert_eq!(query_param(shown, "client_id").as_deref(), Some("c1"));
        assert_eq!(
            query_param(shown, "code_challenge_method").as_deref(),
            Some("S256")
        );
        assert!(query_param(shown, "code_challenge").is_some(), "{shown}");
        assert!(query_param(shown, "state").is_some(), "{shown}");
        assert!(query_param(shown, "redirect_uri").is_some(), "{shown}");
        succeed(shown)
    })
    .unwrap();
}

#[test]
fn the_authorization_url_appends_with_the_separator_the_endpoint_needs() {
    let plain = super::authorization_url(
        "https://auth.test/authorize",
        "c1",
        "http://127.0.0.1:1/callback",
        "challenge",
        "state",
        "https://rs.test/mcp",
    );
    assert!(
        plain.starts_with("https://auth.test/authorize?response_type=code&"),
        "{plain}"
    );
    // An endpoint that already carries a query keeps it whole: the
    // parameters join with `&`, or with no separator when the endpoint
    // already ends mid-query, never with a second `?`.
    for (endpoint, joined) in [
        (
            "https://auth.test/authorize?tenant=x",
            "?tenant=x&response_type=",
        ),
        ("https://auth.test/authorize?", "?response_type="),
        (
            "https://auth.test/authorize?tenant=x&",
            "?tenant=x&response_type=",
        ),
    ] {
        let url = super::authorization_url(
            endpoint,
            "c1",
            "http://127.0.0.1:1/callback",
            "challenge",
            "state",
            "https://rs.test/mcp",
        );
        assert!(url.contains(joined), "{url}");
        assert!(!url.contains("?tenant=x?"), "{url}");
    }
}

#[test]
fn the_challenge_is_s256_of_the_verifier() {
    // The authorization URL's challenge equals base64url(SHA-256(the
    // `code_verifier` the token request later carries)): flipping either
    // side breaks the pair.
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let challenge_seen = std::sync::Mutex::new(String::new());
    run_with_callback(&url, &files, &clock, |shown| {
        *challenge_seen.lock().unwrap() = query_param(shown, "code_challenge").unwrap();
        succeed(shown)
    })
    .unwrap();
    let exchange = server
        .requests()
        .into_iter()
        .find(|request| request.path == "/token")
        .unwrap();
    let verifier = String::from_utf8_lossy(&exchange.body)
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == "code_verifier").then(|| decode(value))
        })
        .unwrap();
    let expected = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()),
    );
    assert_eq!(challenge_seen.lock().unwrap().as_str(), expected);
}

#[test]
fn the_stored_record_holds_the_token_the_endpoint_and_the_url() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let stored = files
        .read(&crate::token::token_key(&url).unwrap())
        .unwrap()
        .unwrap();
    let canonical = crate::token::canonical_url(&url).unwrap();
    assert_eq!(stored["access_token"], json!("at-1"));
    assert_eq!(stored["refresh_token"], json!("rt-1"));
    assert_eq!(
        stored["token_endpoint"],
        json!(format!("{}/token", server.url()))
    );
    assert_eq!(stored["client_id"], json!("c1"));
    assert_eq!(stored["url"], json!(canonical));
    let wall_secs = contract::clock::wall_ms(clock.wall()) / 1000;
    assert_eq!(stored["expires_at"], json!(wall_secs + 3600));
}

#[test]
fn without_expires_in_no_expiry_is_stored() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => json_response(&json!({
                "access_token": "at-1",
                "token_type": "Bearer",
            })),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let stored = files
        .read(&crate::token::token_key(&url).unwrap())
        .unwrap()
        .unwrap();
    assert!(stored.get("expires_at").is_none(), "{stored}");
}

#[test]
fn bad_token_replies_fail_and_store_nothing() {
    for (reply, what) in [
        (
            json!({"token_type": "Bearer"}).to_string(),
            "no access token",
        ),
        (
            json!({"access_token": "at-1", "token_type": "mac"}).to_string(),
            "non-Bearer type",
        ),
        ("not json secret".to_owned(), "non-JSON"),
    ] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    json_response(&json!({"authorization_servers": [base.clone()]}))
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&authorization_document(&base))
                }
                ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
                ("POST", "/token") => Response::status(200, reply.clone()),
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        let error = run_with_callback(&url, &files, &clock, succeed).unwrap_err();
        assert!(
            matches!(error, LoginError::Exchange(_)),
            "{what}: {error:?}"
        );
        assert!(!error.to_string().contains("secret"), "{what}: {error}");
        assert!(
            files
                .read(&crate::token::token_key(&url).unwrap())
                .unwrap()
                .is_none(),
            "{what}"
        );
    }
}

#[test]
fn exchange_failures_are_terminal_with_no_second_request() {
    for case in ["status", "dropped", "malformed"] {
        let server = ProviderServer::start_responding(move |request: &Request| {
            let base = base_of(request);
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mcp") => Response::status(401, "{}"),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    json_response(&json!({"authorization_servers": [base.clone()]}))
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&authorization_document(&base))
                }
                ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
                ("POST", "/token") => match case {
                    "status" => Response::status(500, "secret-body"),
                    "dropped" => Response::drop_connection(),
                    _ => Response::status(200, "not json"),
                },
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        let error = run_with_callback(&url, &files, &clock, succeed).unwrap_err();
        assert!(
            matches!(error, LoginError::Exchange(_)),
            "{case}: {error:?}"
        );
        assert!(
            !error.to_string().contains("secret-body"),
            "{case}: {error}"
        );
        let exchanges = server
            .requests()
            .into_iter()
            .filter(|request| request.path == "/token")
            .count();
        assert_eq!(exchanges, 1, "{case}: the exchange retried");
        assert!(
            files
                .read(&crate::token::token_key(&url).unwrap())
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
}

#[test]
fn a_callback_with_the_wrong_state_fails_and_stores_nothing() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = run_with_callback(&url, &files, &clock, |url| {
        let state = query_param(url, "state").unwrap();
        format!("code=abc&state={state}x")
    })
    .unwrap_err();
    assert!(matches!(error, LoginError::Callback(_)), "{error:?}");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path != "/token")
    );
}

#[test]
fn a_callback_error_fails_denied_printing_only_the_code() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = run_with_callback(&url, &files, &clock, |url| {
        let state = query_param(url, "state").unwrap();
        format!("error=access_denied&error_description=secret&state={state}")
    })
    .unwrap_err();
    assert!(matches!(error, LoginError::Denied(_)), "{error:?}");
    assert!(error.to_string().contains("access_denied"), "{error}");
    assert!(!error.to_string().contains("secret"), "{error}");
    assert!(!format!("{error:?}").contains("secret"), "{error:?}");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_unknown_callback_error_prints_the_generic_sentence() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    // `state` echoed as the error value prints no `state`: the generic
    // sentence never repeats the value.
    let error = run_with_callback(&url, &files, &clock, |url| {
        let state = query_param(url, "state").unwrap();
        format!("error={state}&state={state}")
    })
    .unwrap_err();
    assert!(matches!(error, LoginError::Denied(_)), "{error:?}");
    assert_eq!(
        error.to_string(),
        "The authorization server returned an error."
    );
}

#[test]
fn a_request_for_another_path_gets_404_and_the_wait_continues() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, |shown| {
        let redirect = query_param(shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        let mut other = TcpStream::connect(host).unwrap();
        other
            .write_all(
                format!("GET /other HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        let mut reply = String::new();
        other.read_to_string(&mut reply).unwrap();
        assert!(reply.contains("404"), "{reply}");
        succeed(shown)
    })
    .unwrap();
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_head_past_8kib_gets_431_and_the_wait_continues() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, |shown| {
        let redirect = query_param(shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        // A head of exactly 8 KiB is past the cap; one byte less reads.
        // The refusal may arrive as a 431 or as a reset connection: the
        // server closed with the request half-read, and either way the
        // wait continues for the real callback below.
        let mut big = TcpStream::connect(host).unwrap();
        // No socket timeout: the read ends at the close the listener
        // performs after writing the 431, and a hang is bounded by the
        // login result's own deadline with its fake-clock advance in
        // `run_with_callback`.
        let padding = "x".repeat(MAX_HEAD);
        let _written = big.write_all(
            format!("GET /callback?{padding} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
        );
        let mut reply = String::new();
        if big.read_to_string(&mut reply).is_ok() {
            assert!(reply.contains("431"), "{reply}");
        }
        succeed(shown)
    })
    .unwrap();
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_busy_store_is_a_store_failure() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let key = crate::token::token_key(&url).unwrap();
    let _held = files.try_lock(&key).unwrap().unwrap();
    let error = run_with_callback(&url, &files, &clock, succeed).unwrap_err();
    assert!(matches!(error, LoginError::Store(_)), "{error:?}");
}

#[test]
fn an_idle_connection_is_dropped_and_a_later_callback_succeeds() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, |shown| {
        let redirect = query_param(shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        // An idle connection, opened with no bytes: the listener drops it
        // after its silent polls, which this read sees as the close.
        let mut idle = TcpStream::connect(host).unwrap();
        // No socket timeout: the read ends at the close the listener
        // performs after its silent polls, and a hang is bounded by the
        // login result's own deadline with its fake-clock advance in
        // `run_with_callback`.
        let mut byte = [0_u8; 1];
        assert_eq!(
            idle.read(&mut byte).unwrap(),
            0,
            "the idle connection stayed open"
        );
        succeed(shown)
    })
    .unwrap();
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn without_a_callback_the_wait_fails_at_900s_and_stores_nothing() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let (sent_url, received_url) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    std::thread::scope(|scope| {
        let files = &files;
        let moved = Arc::clone(&clock);
        let login = scope.spawn(move || {
            let result = login(
                files,
                &url,
                &|url| sent_url.send(url.to_owned()).unwrap(),
                &*moved,
            );
            sent_done.send(result).unwrap();
        });
        // The authorization URL is shown before the wait.
        fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        clock.advance_when_parked(
            Some(deadline),
            None,
            CALLBACK_WAIT,
            &fakes::Deadline::after(WITHIN),
        );
        let result =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the timed-out login");
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        login.join().unwrap();
    });
    assert!(
        files
            .read(&crate::token::token_key(&server_url(&server)).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn at_899s_the_wait_continues_and_at_900s_it_fails() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let (sent_url, received_url) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    std::thread::scope(|scope| {
        let files = &files;
        let moved = Arc::clone(&clock);
        let login = scope.spawn(move || {
            let result = login(
                files,
                &url,
                &|url| sent_url.send(url.to_owned()).unwrap(),
                &*moved,
            );
            sent_done.send(result).unwrap();
        });
        fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        // Still waiting: the login parks on the clock again after the
        // advance, so the last second still ends it at 900 s. The mark
        // is each thread's latest park when the clock moves; the await
        // needs a later park with the same deadline, so a mutant that
        // ends the wait early never re-parks and fails here. The park
        // is the code's own signal; no real-clock receive expiry paces
        // this test.
        let mark = clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::from_secs(899),
            &fakes::Deadline::after(WITHIN),
        );
        assert!(
            clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(WITHIN)),
            "the login ended before 900 s"
        );
        clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::from_secs(1),
            &fakes::Deadline::after(WITHIN),
        );
        let result =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the timed-out login");
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        login.join().unwrap();
    });
}

#[test]
fn a_slow_drip_does_not_stop_the_900s_deadline() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let (sent_url, received_url) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    std::thread::scope(|scope| {
        let files = &files;
        let moved = Arc::clone(&clock);
        let login = scope.spawn(move || {
            let result = login(
                files,
                &url,
                &|url| sent_url.send(url.to_owned()).unwrap(),
                &*moved,
            );
            sent_done.send(result).unwrap();
        });
        let shown =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        assert!(
            clock.await_parked(deadline, fakes::Deadline::after(WITHIN)),
            "the login never waited on the clock"
        );
        // A slow drip: one byte at a time, never completing a head. Each
        // byte the callback reader consumes taps the test, so the first
        // tap proves the connection was accepted before the clock moves,
        // and each later tap releases the next byte: no pacing sleep.
        // The redirect is read inside the releasing scope: a value mutant
        // that breaks the authorization URL panics there, releasing the
        // parked login before the scope joins, so the mutant fails fast
        // instead of hanging the suite.
        let reads = super::callback::tap_reads();
        let drips = fakes::Deadline::after(WITHIN);
        // `drip` returns to the test and stays open across the advance
        // below: the deadline interrupts the reader's blocked read, not
        // an EOF from a dropped connection.
        let drip = drive_releasing_on_panic(&clock, || {
            let redirect = query_param(&shown, "redirect_uri").unwrap().to_owned();
            let (_, rest) = redirect.split_once("://").unwrap();
            let (host, _) = rest.split_once('/').unwrap();
            let mut drip = TcpStream::connect(host).unwrap();
            drip.write_all(b"G").unwrap();
            drips.recv_or_fail(&reads, "the callback reader's first byte");
            for _ in 0..60 {
                drip.write_all(b"G").unwrap();
                drips.recv_or_fail(&reads, "the callback reader's next byte");
            }
            drip
        });
        clock.advance_when_parked(
            Some(deadline),
            None,
            CALLBACK_WAIT,
            &fakes::Deadline::after(WITHIN),
        );
        let result =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the timed-out login");
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        login.join().unwrap();
        drop(drip);
    });
    assert!(
        files
            .read(&crate::token::token_key(&server_url(&server)).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_body_cap_is_one_mib() {
    assert_eq!(super::MAX_BODY, 1024 * 1024);
}

#[test]
fn encode_percent_encodes_everything_outside_the_unreserved_set() {
    assert_eq!(super::encode("abcXYZ019-_.~"), "abcXYZ019-_.~");
    assert_eq!(super::encode(""), "");
    assert_eq!(super::encode("a b+c/d?e=f&g"), "a%20b%2Bc%2Fd%3Fe%3Df%26g");
    assert_eq!(super::encode("é"), "%C3%A9");
}

#[test]
fn authorization_url_is_the_exact_joined_query() {
    assert_eq!(
        super::authorization_url(
            "https://auth.test/authorize",
            "c1",
            "http://127.0.0.1:9/callback",
            "challenge",
            "state",
            "https://rs.test/mcp",
        ),
        "https://auth.test/authorize?response_type=code&client_id=c1&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback&code_challenge=challenge&code_challenge_method=S256&state=state&resource=https%3A%2F%2Frs.test%2Fmcp"
    );
}

#[test]
fn debug_prints_each_variant_name_only() {
    use crate::token::TokenError;
    for (error, name) in [
        (LoginError::Url, "Url"),
        (LoginError::Userinfo, "Userinfo"),
        (LoginError::Unreachable("step".to_owned()), "Unreachable"),
        (LoginError::Random, "Random"),
        (LoginError::Discovery("step".to_owned()), "Discovery"),
        (LoginError::Registration("step".to_owned()), "Registration"),
        (LoginError::Exchange("step".to_owned()), "Exchange"),
        (LoginError::Callback("step".to_owned()), "Callback"),
        (LoginError::Denied("access_denied".to_owned()), "Denied"),
        (LoginError::PkceUnsupported, "PkceUnsupported"),
        (LoginError::Timeout, "Timeout"),
    ] {
        assert_eq!(format!("{error:?}"), name);
    }
    assert_eq!(
        format!("{:?}", LoginError::Store(TokenError::Url)),
        "Store(Url)"
    );
}

#[test]
fn post_helpers_read_one_mib_plus_one_whole() {
    // The `+ 1` reads one byte past the cap, so a body of exactly
    // `MAX_BODY + 1` still arrives whole and the callers see its length.
    let big = "b".repeat(super::MAX_BODY + 1);
    let server =
        ProviderServer::start_responding(move |_: &Request| Response::status(200, big.clone()))
            .unwrap();
    let url = format!("{}/reply", server.url());
    let observe = super::Observe {
        debug: None,
        clock: &*fakes::clock::FakeClock::new(),
    };
    let (status, bytes) = super::post_json(&url, "{}", &observe).unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes.len(), super::MAX_BODY + 1);
    assert_eq!(bytes[0], b'b');
    assert_eq!(bytes[super::MAX_BODY], b'b');
    let (status, bytes) = super::post_form(&url, &[("a", "b")], &observe).unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes.len(), super::MAX_BODY + 1);
}

/// `value`'s JSON padded with spaces to exactly `size` bytes: still JSON,
/// still carrying its fields.
fn padded(value: &Value, size: usize) -> String {
    let mut body = value.to_string();
    body.extend(std::iter::repeat_n(' ', size.saturating_sub(body.len())));
    body
}

/// A fake whose registration answers `register` at `/register` while the
/// rest of the flow is `start_full`'s.
fn start_registering(register: String) -> ProviderServer {
    ProviderServer::start_responding(move |request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => Response::status(200, register.clone()),
            ("POST", "/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap()
}

/// A fake whose token endpoint answers `token` at `/token` while the rest
/// of the flow is `start_full`'s.
fn start_exchanging(token: String, status: u16) -> ProviderServer {
    ProviderServer::start_responding(move |request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/token") => Response::status(status, token.clone()),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap()
}

#[test]
fn register_rejects_a_non_2xx_with_a_valid_body() {
    // A 500 whose body would otherwise register `c1`: the status alone
    // fails registration, before any callback.
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                json_response(&authorization_document(&base))
            }
            ("POST", "/register") => Response::status(500, json!({"client_id": "c1"}).to_string()),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Registration(_)), "{error:?}");
}

#[test]
fn register_reads_a_body_of_exactly_one_mib() {
    let server = start_registering(padded(&json!({"client_id": "c1"}), super::MAX_BODY));
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn register_rejects_a_body_past_one_mib() {
    let server = start_registering(padded(&json!({"client_id": "c1"}), super::MAX_BODY + 1));
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let error = login(
        &files,
        &server_url(&server),
        &|_| panic!("no authorization URL is shown"),
        &*clock,
    )
    .unwrap_err();
    assert!(matches!(error, LoginError::Registration(_)), "{error:?}");
}

#[test]
fn exchange_rejects_a_non_2xx_with_a_valid_body() {
    // A 500 whose body would otherwise store `at-1`: the status alone
    // fails the exchange and stores nothing.
    let server = start_exchanging(token_reply("at-1").to_string(), 500);
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = run_with_callback(&url, &files, &clock, succeed).unwrap_err();
    assert!(matches!(error, LoginError::Exchange(_)), "{error:?}");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn exchange_reads_a_body_of_exactly_one_mib() {
    let server = start_exchanging(padded(&token_reply("at-1"), super::MAX_BODY), 200);
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn exchange_rejects_a_body_past_one_mib() {
    let server = start_exchanging(padded(&token_reply("at-1"), super::MAX_BODY + 1), 200);
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let error = run_with_callback(&url, &files, &clock, succeed).unwrap_err();
    assert!(matches!(error, LoginError::Exchange(_)), "{error:?}");
    assert!(
        files
            .read(&crate::token::token_key(&url).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_header_document_is_used_when_the_well_known_ones_are_absent() {
    // The probe's header names `/prm`, whose document lists the
    // path-bearing `{base}/tenant` issuer; the well-known
    // protected-resource documents are absent or name a dead server, so a
    // login that ignores the header fails discovery.
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        let issuer = format!("{base}/tenant");
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}").header(
                "www-authenticate",
                &format!("Bearer resource_metadata=\"{base}/prm\""),
            ),
            ("GET", "/prm") => json_response(&json!({"authorization_servers": [issuer]})),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => Response::status(404, "{}"),
            ("GET", "/.well-known/oauth-protected-resource") => {
                json_response(&json!({"authorization_servers": ["http://127.0.0.1:1/dead"]}))
            }
            ("GET", "/.well-known/oauth-authorization-server/tenant") => {
                json_response(&authorization_document(&issuer))
            }
            ("POST", "/tenant/register") => json_response(&json!({"client_id": "c1"})),
            ("POST", "/tenant/token") => json_response(&token_reply("at-1")),
            _ => Response::status(404, "{}"),
        }
    })
    .unwrap();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let paths: Vec<String> = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(
        paths.contains(&"/prm".to_owned()),
        "the header's document was not fetched: {paths:?}"
    );
    let stored = files
        .read(&crate::token::token_key(&url).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored["token_endpoint"],
        json!(format!("{}/tenant/token", server.url()))
    );
}

/// Sends raw `head` bytes to `addr` and reads the reply to the close,
/// never failing: a refused connection or a reset read is an empty reply,
/// so no assertion inside the driving scope can hang the wait. No socket
/// timeout: the read ends at the close the server performs when it
/// answers, and the half-close below guarantees that close even when the
/// server reads past the head, with the fake deadline ending the wait.
fn send_raw(addr: std::net::SocketAddr, head: &[u8]) -> String {
    let mut stream = match TcpStream::connect(addr) {
        Ok(stream) => stream,
        Err(_) => return String::new(),
    };
    let _written = stream.write_all(head);
    let _half_closed = stream.shutdown(std::net::Shutdown::Write);
    let mut reply = String::new();
    let _read = stream.read_to_string(&mut reply);
    reply
}

/// Drives one callback wait to its end: the wait runs on a scoped
/// thread while `interact` works the listener's address, then the result
/// is received with a bound, advancing the fake clock past the deadline
/// when the bound expires, so a mutant that never answers still ends the
/// wait and the scope still joins. The clock advances only after the wait
/// parks on it below, so the advance never ends a wait that has not
/// started. `interact` reports through its echo, asserted after the join.
fn drive_wait(
    clock: &Arc<fakes::clock::FakeClock>,
    deadline: std::time::Instant,
    state: &str,
    silent_polls: u32,
    interact: impl FnOnce(std::net::SocketAddr) -> String,
) -> (Result<String, LoginError>, String) {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    // Nonblocking, as the login's own listener is: the wait parks on the
    // clock between accepts, so advancing the clock always ends it.
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let result = super::callback::wait_callback_within(
                &listener,
                &**clock,
                deadline,
                state,
                silent_polls,
            );
            sent.send(result).unwrap();
        });
        // The wait parks on the fake clock between accepts: only after
        // this signal does the test interact, and only after it may the
        // fallback below advance the clock.
        assert!(
            clock.await_parked(deadline, fakes::Deadline::after(WITHIN)),
            "the callback wait never waited on the clock"
        );
        let echo = interact(addr);
        let result = match fakes::Deadline::after(WITHIN).recv(&received) {
            Ok(result) => result,
            Err(_) => {
                clock.advance_when_parked(
                    Some(deadline),
                    None,
                    deadline.saturating_duration_since(clock.now()),
                    &fakes::Deadline::after(WITHIN),
                );
                fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the released callback wait")
            }
        };
        waiter.join().unwrap();
        (result, echo)
    })
}

fn driven_wait(
    clock: &Arc<fakes::clock::FakeClock>,
    deadline: std::time::Instant,
    state: &str,
    interact: impl FnOnce(std::net::SocketAddr) -> String,
) -> (Result<String, LoginError>, String) {
    driven_wait_within(clock, deadline, state, SILENT_POLLS, interact)
}

/// [`drive_wait`] with the silent-poll count as a parameter, so the idle
/// test runs a small count while production keeps `SILENT_POLLS`.
fn driven_wait_within(
    clock: &Arc<fakes::clock::FakeClock>,
    deadline: std::time::Instant,
    state: &str,
    silent_polls: u32,
    interact: impl FnOnce(std::net::SocketAddr) -> String,
) -> (Result<String, LoginError>, String) {
    drive_wait(clock, deadline, state, silent_polls, interact)
}

#[test]
fn the_callback_wait_answers_one_valid_request() {
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let (result, echo) = driven_wait(&clock, deadline, "s1", |addr| {
        send_raw(
            addr,
            b"GET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
    });
    assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
    assert_eq!(result.unwrap(), "abc");
}

#[test]
fn the_callback_head_cap_is_8kib() {
    assert_eq!(super::callback::MAX_HEAD, 8 * 1024);
}

/// A callback head of exactly `size` bytes: the request line carries the
/// valid callback, and `X-Pad` stretches the head to the byte.
fn sized_head(size: usize) -> Vec<u8> {
    let prefix = b"GET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nX-Pad: ";
    let suffix = b"\r\n\r\n";
    let mut head = Vec::with_capacity(size);
    head.extend_from_slice(prefix);
    head.extend(std::iter::repeat_n(
        b'p',
        size - prefix.len() - suffix.len(),
    ));
    head.extend_from_slice(suffix);
    assert_eq!(head.len(), size);
    head
}

/// Calls [`answer`] once with `head` already written: the client writes
/// before the call, so no thread and no clock wait is involved, and the
/// reply is read after it returns. The fake clock never moves and no
/// deadline is waited out: a mutant of the answer or the cap fails on the
/// assertions below, never on a real-clock timeout.
fn answer_once(head: &[u8], state: &str) -> (Option<Served>, String) {
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).unwrap();
    client.write_all(head).unwrap();
    let (server, _) = listener.accept().unwrap();
    let served = answer(server, &*clock, deadline, state);
    let mut reply = String::new();
    let _read = client.read_to_string(&mut reply);
    (served, reply)
}

#[test]
fn answer_reads_a_head_of_exactly_the_cap() {
    // The cap check runs before each read, so a head of exactly the 8 KiB
    // cap still reads whole: a `<` mutant of the `>=` refuses it too, and
    // an `answer` mutant that never answers returns `None` for it. The
    // size is a literal, not `MAX_HEAD`, so a narrowed-cap mutant cannot
    // underflow `sized_head` before the assertion;
    // `the_callback_head_cap_is_8kib` pins the value itself.
    const CAP: usize = 8 * 1024;
    let head = sized_head(CAP);
    let (served, echo) = answer_once(&head, "s1");
    assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
    match served {
        Some(Served::Code(code)) => assert_eq!(code, "abc"),
        _ => panic!("a head of exactly the cap did not read"),
    }
}

#[test]
fn an_over_cap_head_is_refused_with_431_and_the_next_callback_succeeds() {
    // One byte past the 8 KiB cap is refused with 431 and the next head
    // still reads: the refused reply is mandatory, and the second 200
    // with the code proves the wait continued. Both calls are direct, one
    // head fully written before each call with the clock never moving, so
    // an `answer` mutant that never answers fails the assertions below
    // instead of waiting out a result deadline; the server drains the
    // refused head before closing, so the 431 cannot race the close into
    // a reset.
    const CAP: usize = 8 * 1024;
    let over = sized_head(CAP + 1);
    let (refused, first) = answer_once(&over, "s1");
    assert!(first.starts_with("HTTP/1.1 431"), "{first}");
    assert!(refused.is_none(), "a refused head ended the wait");
    let (served, second) = answer_once(
        b"GET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        "s1",
    );
    assert!(second.starts_with("HTTP/1.1 200"), "{second}");
    match served {
        Some(Served::Code(code)) => assert_eq!(code, "abc"),
        _ => panic!("the callback after a refused head did not read"),
    }
}

#[test]
fn the_drain_ends_at_twice_the_cap() {
    // The drain reads at most one more cap past the refused head:
    // `while head.len() < 2 * MAX_HEAD`. One byte under the second cap
    // the drain still loops: with nothing more arriving it parks on the
    // injected clock, and passing the deadline ends it with the timeout.
    // Exactly at the second cap the loop ends at once: the answer is
    // `None` without waiting on the clock. A `<=` mutant loops once more
    // at the cap: it parks, and the timeout below fails the `None`
    // assertion. Both sizes are literals, not `MAX_HEAD`, so a
    // narrowed-cap mutant cannot underflow the vectors before the
    // assertions; `the_callback_head_cap_is_8kib` pins the value itself.
    const TWICE: usize = 2 * 8 * 1024;
    // One byte under the second cap, no head end, connection open: the
    // drain consumes every byte, parks for more, and the advance past
    // the deadline ends it with the timeout.
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(&vec![b'G'; TWICE - 1]).unwrap();
        let (server, _) = listener.accept().unwrap();
        let answerer = scope.spawn(|| {
            sent.send(answer(server, &*clock, deadline, "s1")).unwrap();
        });
        // The drain consumed every byte and parks for more: only after
        // this signal may the advance below move past the deadline, so
        // the timeout it returns is the drain's, not a wait that never
        // started.
        clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::from_secs(60),
            &fakes::Deadline::after(WITHIN),
        );
        let served =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the parked drain's answer");
        match served {
            Some(Served::Failed(error)) => {
                assert!(matches!(error, LoginError::Timeout), "{error:?}");
            }
            _ => panic!("a drain one byte under the second cap did not time out"),
        }
        answerer.join().unwrap();
    });
    // Exactly at the second cap, no head end, connection open: the drain
    // ends at once with `None`, without parking on the clock. A `<=`
    // mutant reads once more, parks for the byte past the cap, and the
    // advance below turns its answer into the timeout, which fails the
    // `None` assertion.
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(&vec![b'G'; TWICE]).unwrap();
        let (server, _) = listener.accept().unwrap();
        let answerer = scope.spawn(|| {
            sent.send(answer(server, &*clock, deadline, "s1")).unwrap();
        });
        let served = match fakes::Deadline::after(WITHIN).recv(&received) {
            Ok(served) => served,
            Err(_) => {
                clock.advance_when_parked(
                    Some(deadline),
                    None,
                    Duration::from_secs(60),
                    &fakes::Deadline::after(WITHIN),
                );
                fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the capped drain's answer")
            }
        };
        assert!(served.is_none(), "a drain at the second cap did not end");
        answerer.join().unwrap();
    });
}

#[test]
fn a_reset_in_the_drain_breaks_the_drain() {
    // A read error whose kind is not `WouldBlock` ends the drain at
    // once: the peer writes past the cap, never reads the 431, and
    // closes with the whole reply unread, so the close carries a
    // reset on every platform and the drain's next read fails instead of
    // reading a clean close, and the answer is `None` with no further
    // park on the clock. A `true` guard parks on the reset instead of
    // breaking, and the advance below turns its answer into the
    // timeout, which fails the `None` assertion.
    const CAP: usize = 8 * 1024;
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(addr).unwrap();
        // Past the cap with no head end: the server answers 431, then
        // drains a head that never terminates on its own.
        client.write_all(&vec![b'G'; CAP + 1024]).unwrap();
        let (server, _) = listener.accept().unwrap();
        let answerer = scope.spawn(|| {
            sent.send(answer(server, &*clock, deadline, "s1")).unwrap();
        });
        // The drain holds `answer`'s only park: this signal proves the
        // server wrote the 431 and consumed every sent byte, so the
        // reset below meets an empty receive buffer and the drain's
        // next read fails at once instead of consuming a byte. No
        // socket read proves the 431: the client never reads it, so the
        // whole reply stays unread in its own buffer.
        let mark = clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::ZERO,
            &fakes::Deadline::after(WITHIN),
        );
        // The whole 431 stays unread in the client's buffer, so the
        // close carries a reset on Linux and macOS alike, never a
        // clean close: the drain's next read fails, it does not read
        // `Ok(0)`.
        drop(client);
        let served = match fakes::Deadline::after(WITHIN).recv(&received) {
            Ok(served) => served,
            Err(_) => {
                clock.advance_when_parked(
                    Some(deadline),
                    None,
                    Duration::from_secs(60),
                    &fakes::Deadline::after(WITHIN),
                );
                fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the reset head's answer")
            }
        };
        assert!(served.is_none(), "a reset head in the drain ended the wait");
        answerer.join().unwrap();
        // No code under test waits on the clock here; the answering thread is joined, so this zero-length move only snapshots the parks (ruling 6179406086).
        assert_eq!(
            {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "no code under test waits on the clock (ruling 6179406086)"
                )]
                let actual = clock.advance_marked(Duration::ZERO);
                actual
            },
            mark,
            "the drain parked after the reset instead of breaking"
        );
    });
}

#[test]
fn a_refused_head_dripped_past_the_deadline_ends_at_the_deadline() {
    // A peer sends an over-cap head with no terminator, reads the 431,
    // then keeps dripping after the fake deadline has passed: the drain
    // stops at the deadline with the timeout instead of consuming the
    // drip. Reading the 431 first proves the server reached the drain, so
    // the advance below lands mid-drain; without the drain's deadline
    // check the answer is `None`, after the drip.
    const CAP: usize = 8 * 1024;
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut client = TcpStream::connect(addr).unwrap();
        // Over the cap with no head end: the server answers 431, then
        // drains a head that never terminates on its own.
        client.write_all(&vec![b'G'; CAP + 1]).unwrap();
        let (server, _) = listener.accept().unwrap();
        let answerer = scope.spawn(|| {
            sent.send(answer(server, &*clock, deadline, "s1")).unwrap();
        });
        // The 431 arrives before the drain, whole: one worker reads to
        // its end, bounded by one wall-clock deadline, so a refused head
        // that never ends still ends here. On failure the socket is shut
        // down before joining, so the worker's blocking read ends and
        // the scope still joins.
        let (sent_reply, received_reply) = mpsc::channel();
        let shutdown = client.try_clone().unwrap();
        let mut writer = client.try_clone().unwrap();
        let reader = scope.spawn(move || {
            let mut reply = Vec::new();
            let mut byte = [0_u8; 1];
            while !reply.ends_with(b"\r\n\r\n") {
                match client.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => reply.push(byte[0]),
                }
            }
            sent_reply.send(reply).unwrap();
        });
        let reply = match fakes::Deadline::after(WITHIN).recv(&received_reply) {
            Ok(reply) => reply,
            Err(_) => {
                let _shutdown = shutdown.shutdown(std::net::Shutdown::Both);
                fakes::Deadline::after(WITHIN)
                    .recv_or_fail(&received_reply, "the refused head's 431")
            }
        };
        reader.join().unwrap();
        let reply = String::from_utf8_lossy(&reply).into_owned();
        assert!(reply.starts_with("HTTP/1.1 431"), "{reply}");
        // The drain parks on the injected clock while it waits for more
        // of the refused head: each drip below is released only after a
        // later park proves the drain consumed the previous one, so the
        // clock moves past the deadline while the drain is provably
        // inside its deadline-bounded wait, and the answer is the
        // deadline's timeout, not a kernel-timeout race. A plain
        // `await_parked` could match one park for every drip; the mark
        // needs a later park after each write, so a drip the drain never
        // consumed fails here. Without the drain's deadline check the
        // answer never comes and the bound below fails the test.
        for _ in 0..8 {
            // Advancing by zero moves no deadline: it only snapshots each
            // thread's latest park, so the await needs a park after the
            // write below.
            let mark = clock.advance_when_parked(
                Some(deadline),
                None,
                Duration::ZERO,
                &fakes::Deadline::after(WITHIN),
            );
            writer.write_all(b"G").unwrap();
            assert!(
                clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(WITHIN)),
                "the drain never waited on the clock"
            );
        }
        let mark = clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::ZERO,
            &fakes::Deadline::after(WITHIN),
        );
        assert!(
            clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(WITHIN)),
            "the drain never waited on the clock"
        );
        clock.advance_when_parked(
            Some(deadline),
            None,
            Duration::from_secs(60),
            &fakes::Deadline::after(WITHIN),
        );
        let served =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the refused head's answer");
        match served {
            Some(Served::Failed(error)) => {
                assert!(matches!(error, LoginError::Timeout), "{error:?}");
            }
            _ => panic!("a refused head dripped past the deadline did not time out"),
        }
        answerer.join().unwrap();
    });
}

#[test]
fn answer_fails_a_garbage_head_with_the_request_error() {
    // A head that is no request ends with the `callback request` failure:
    // an `answer` mutant that never answers returns `None` instead.
    let (served, echo) = answer_once(b"not a request\r\n\r\n", "s1");
    assert!(echo.starts_with("HTTP/1.1 400"), "{echo}");
    match served {
        Some(Served::Failed(error)) => {
            assert!(matches!(error, LoginError::Callback(_)), "{error:?}");
            assert_eq!(
                error.to_string(),
                "Fiber could not read the browser callback at callback request."
            );
        }
        _ => panic!("a garbage head did not end with the request error"),
    }
}

#[test]
fn a_mid_size_callback_head_reads() {
    // A head of about 2 KiB is far under the 8 KiB cap, and far past a
    // narrowed one: it reads whole.
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let padding = "p".repeat(1900);
    let head = format!(
        "GET /callback?code=abc&state=s1&pad={padding} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    );
    let (result, echo) = driven_wait(&clock, deadline, "s1", |addr| {
        send_raw(addr, head.as_bytes())
    });
    assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
    assert_eq!(result.unwrap(), "abc");
}

/// A connection silent for the given count of reads is dropped on that
/// poll, and a connection that pauses shorter is served: the boundary
/// and the byte-reset through `answer_within`, paced by the reader's own
/// tap signals, never the real clock. Exact counts hold because each test
/// runs in its own process (docs/testing.md, "Running tests"), so no
/// other test's connection can signal this test's taps.
#[test]
fn a_connection_silent_for_the_given_count_is_dropped_on_that_poll() {
    // Three silent polls, then the close: the drop comes on the third.
    {
        let clock = fakes::clock::FakeClock::new();
        let deadline = clock.origin() + Duration::from_secs(60);
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let silence = super::callback::tap_silence();
        let idle = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let (sent, received) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                sent.send(super::callback::answer_within(
                    server, &*clock, deadline, "s1", 3,
                ))
                .unwrap();
            });
            // One deadline for the whole wait: three silent polls, then
            // the close. A `>` mutant of the count drops only on a
            // fourth poll, so its fourth signal fails the drain below.
            let wait = fakes::Deadline::after(WITHIN);
            wait.recv_or_fail(&silence, "the first silent poll");
            wait.recv_or_fail(&silence, "the second silent poll");
            wait.recv_or_fail(&silence, "the third silent poll");
            // The drop closes the connection: read to the close.
            fakes::within(
                "the dropped connection's close",
                Deadline::after(WITHIN),
                || {
                    let mut idle = idle;
                    let mut byte = [0_u8; 1];
                    while idle.read(&mut byte).expect("the idle connection's close") != 0 {}
                },
            );
            let served = wait.recv_or_fail(&received, "the dropped connection's answer");
            assert!(served.is_none(), "a silent connection is dropped");
            // No fourth poll: the drop came on the third.
            let mut extra = 0;
            while silence.try_recv().is_ok() {
                extra += 1;
            }
            assert_eq!(extra, 0, "the drop is on the third silent poll");
        });
    }
    // Sixteen silent polls with a byte in the middle are served: any
    // byte restarts the count, so eight and eight never reach sixteen.
    // Without the reset the count reaches sixteen and the head is
    // refused instead.
    {
        let clock = fakes::clock::FakeClock::new();
        let deadline = clock.origin() + Duration::from_secs(60);
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        // Both taps register before the connection: no byte flows before
        // the test writes one, so no signal is missed between a write
        // and a later registration.
        let silence = super::callback::tap_silence();
        let reads = super::callback::tap_reads();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let (sent, received) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                sent.send(super::callback::answer_within(
                    server, &*clock, deadline, "s1", 16,
                ))
                .unwrap();
            });
            let wait = fakes::Deadline::after(WITHIN);
            // Eight silent polls, then one byte of the head.
            for _ in 0..8 {
                wait.recv_or_fail(&silence, "a silent poll before the byte");
            }
            client.write_all(b"G").unwrap();
            // The byte's tap proves the reader consumed it; only after
            // that signal are the earlier polls discarded, so only
            // post-byte polls count below.
            wait.recv_or_fail(&reads, "the callback reader's byte");
            while silence.try_recv().is_ok() {}
            // Eight more silent polls, then the rest of the head: the
            // count restarted at the byte, so the head is served.
            for _ in 0..8 {
                wait.recv_or_fail(&silence, "a silent poll after the byte");
            }
            client
                .write_all(
                    b"ET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            // One kernel timeout for the reply: the served head answers
            // at once, and a hung answer fails the test instead of
            // hanging it.
            client
                .set_read_timeout(Some(WITHIN))
                .expect("the reply read has a kernel bound");
            let mut echo = String::new();
            client
                .read_to_string(&mut echo)
                .expect("the served callback's reply");
            assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
            let served = wait.recv_or_fail(&received, "the paused connection's answer");
            assert!(
                matches!(served, Some(Served::Code(code)) if code == "abc"),
                "a pausing head is served"
            );
        });
    }
}

/// How long the idle-connection read below waits for the close: one
/// kernel socket timeout, far above the 20 ms `READ_TIMEOUT` the reader
/// polls on, so a read that times out instead of returning 0 fails the
/// test instead of hanging it.
const IDLE_READ: Duration = Duration::from_millis(500);

#[test]
fn the_callback_wait_drops_an_idle_connection_and_serves_the_next() {
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let (result, echo) = driven_wait_within(&clock, deadline, "s1", 10, |addr| {
        // An idle connection, opened with no bytes, then the real
        // callback on a second connection.
        let mut idle = TcpStream::connect(addr).ok();
        let echo = send_raw(
            addr,
            b"GET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        // The wait has served the real callback, so the idle connection
        // is dropped: its read returns 0 at the close.
        if let Some(stream) = idle.as_mut() {
            stream
                .set_read_timeout(Some(IDLE_READ))
                .expect("the idle read has a kernel bound");
            let mut byte = [0_u8; 1];
            while stream.read(&mut byte).expect("the idle connection's close") != 0 {}
        }
        echo
    });
    assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
    assert_eq!(result.unwrap(), "abc");
}

#[test]
fn the_callback_wait_serves_a_connection_that_pauses_before_its_head() {
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.origin() + Duration::from_secs(60);
    let (result, echo) = driven_wait(&clock, deadline, "s1", |addr| {
        let reads = super::callback::tap_reads();
        let mut client = match TcpStream::connect(addr) {
            Ok(client) => client,
            Err(_) => return String::new(),
        };
        let _written = client.write_all(b"G");
        // The head pauses mid-byte: the reader's own signals pace the
        // rest, not the real clock. The first byte's tap proves the
        // connection was accepted; the silence tap is registered only
        // after that byte landed, so its poll provably finds no bytes
        // after it, before the rest arrives.
        fakes::Deadline::after(WITHIN).recv_or_fail(&reads, "the callback reader's first byte");
        let silent = super::callback::tap_silence();
        fakes::Deadline::after(WITHIN).recv_or_fail(&silent, "the callback reader's silent poll");
        let _written = client.write_all(
            b"ET /callback?code=abc&state=s1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        let mut echo = String::new();
        let _read = client.read_to_string(&mut echo);
        echo
    });
    assert!(echo.starts_with("HTTP/1.1 200"), "{echo}");
    assert_eq!(result.unwrap(), "abc");
}

/// A sink that records every `mcp` line: the observed login's record.
struct Lines {
    lines: Mutex<Vec<HttpRequest>>,
}

impl DebugLog for Lines {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, request: &HttpRequest) {
        self.lines.lock().unwrap().push(request.clone());
    }
}

/// Runs the login observed on `debug`, driving one callback as
/// [`run_with_callback`] does: the result and the sink's lines. The
/// silent [`login`] keeps its shape: only the observed entry writes.
fn run_observed_with_callback(
    server_url: &str,
    files: &FakeTokens,
    clock: &Arc<fakes::clock::FakeClock>,
    query_for: impl Fn(&str) -> String + Send,
    debug: Arc<Lines>,
) -> (Result<(), LoginError>, Vec<HttpRequest>) {
    let (sent, received) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    let clock = Arc::clone(clock);
    let (result, recorded) = std::thread::scope(|scope| {
        // `sent` moves into the login thread: when the login fails before
        // presenting, its end drops the URL sender, which releases the
        // driver's blocking receive below. Owned outside, the sender would
        // outlive the scope and the driver would never leave.
        let login = scope.spawn({
            let clock = Arc::clone(&clock);
            let seen = Arc::clone(&debug);
            move || {
                let result = login_observed(
                    files,
                    server_url,
                    &|url| sent.send(url.to_owned()).unwrap(),
                    &*clock,
                    Some(seen as Arc<dyn DebugLog>),
                );
                sent_done.send(result).unwrap();
            }
        });
        let _driver = scope.spawn({
            let clock = Arc::clone(&clock);
            move || {
                // No URL ever comes when the login fails before the wait:
                // the hung-up channel ends the driver, and the result below
                // names the failure.
                let Ok(url) = received.recv() else {
                    return;
                };
                drive_releasing_on_panic(&clock, || {
                    let query = query_for(&url);
                    let redirect = query_param(&url, "redirect_uri").unwrap().to_owned();
                    send_callback(&redirect, &query);
                });
            }
        });
        // The result first, through the deadline: joining a stuck login
        // would hang past the hang guard. On expiry the clock wait is
        // released so the scope can still join.
        let result = match fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&received_done) {
            Ok(result) => result,
            Err(_) => {
                clock.advance_when_parked(
                    Some(clock.origin() + CALLBACK_WAIT),
                    None,
                    CALLBACK_WAIT,
                    &fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                );
                login.join().unwrap();
                panic!("waited until the deadline for the driven login");
            }
        };
        login.join().unwrap();
        (result, debug.lines.lock().unwrap().clone())
    });
    (result, recorded)
}

/// The `mcp` shape every login line carries: the fixed purpose with no
/// identity, host or path, and the first attempt.
fn assert_mcp_shape(line: &HttpRequest) {
    assert_eq!(line.purpose(), Purpose::Mcp);
    assert_eq!(line.identity(), None, "no identity: {line:?}");
    assert_eq!(line.model, None, "no model: {line:?}");
    assert_eq!(line.host, None, "no host: {line:?}");
    assert_eq!(line.path, None, "no path: {line:?}");
    assert_eq!(line.attempt, 1, "every login post sends once: {line:?}");
}

#[test]
fn an_observed_login_writes_one_mcp_line_per_request() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let debug = Arc::new(Lines {
        lines: Mutex::new(Vec::new()),
    });
    let (result, lines) =
        run_observed_with_callback(&server_url(&server), &files, &clock, succeed, debug);
    result.expect("the driven login succeeds");
    let requests = server.requests();
    let paths: Vec<String> = requests
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(
        paths,
        [
            "/mcp",
            "/prm",
            "/.well-known/oauth-authorization-server",
            "/register",
            "/token",
        ],
        "the probe, two discovery fetches, the registration, the exchange"
    );
    // docs/state.md, "Diagnostic logs": one line per POST the transport sends; login, discovery and token refresh alike.
    assert_eq!(lines.len(), requests.len(), "{lines:?}");
    for (line, request) in lines.iter().zip(requests.iter()) {
        assert_mcp_shape(line);
        assert_eq!(
            line.request_bytes,
            request.body.len() as u64,
            "the sent length: {line:?}"
        );
    }
    // The probe answers 401: its `{}` body is read like any fetch.
    assert_eq!(lines[0].status, Some(401), "{:?}", lines[0]);
    assert_eq!(lines[0].request_bytes, 0, "{:?}", lines[0]);
    assert_eq!(lines[0].response_bytes, 2, "{:?}", lines[0]);
    let base = server.url();
    assert_eq!(lines[1].status, Some(200), "{:?}", lines[1]);
    assert_eq!(
        lines[1].response_bytes,
        json!({"authorization_servers": [base]}).to_string().len() as u64,
        "{:?}",
        lines[1]
    );
    assert_eq!(lines[2].status, Some(200), "{:?}", lines[2]);
    assert_eq!(
        lines[2].response_bytes,
        authorization_document(&base).to_string().len() as u64,
        "{:?}",
        lines[2]
    );
    assert_eq!(lines[3].status, Some(200), "{:?}", lines[3]);
    assert_eq!(
        lines[3].response_bytes,
        json!({"client_id": "c1"}).to_string().len() as u64,
        "{:?}",
        lines[3]
    );
    assert_eq!(lines[4].status, Some(200), "{:?}", lines[4]);
    assert_eq!(
        lines[4].response_bytes,
        token_reply("at-1").to_string().len() as u64,
        "{:?}",
        lines[4]
    );
}
