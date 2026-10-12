//! `login`: the error codes, the redaction, discovery and registration,
//! and the authorization, callback and exchange against a routed fake
//! server and a fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::*;
use crate::test_support::{FakeTokens, WITHIN};
use contract::mcp_tokens::TokenFiles;
use fakes::{ProviderServer, Request, Response};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, mpsc};
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
        (LoginError::Timeout, ErrorCode::AuthenticationFailed),
        (
            LoginError::Store(TokenError::Url),
            ErrorCode::InvalidArguments,
        ),
        (
            LoginError::Store(TokenError::Busy),
            ErrorCode::IoFailed,
        ),
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
            ("GET", "/prm") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
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
        (key == name).then(|| super::decode(value))
    })
}

/// Sends one callback GET to `redirect_uri` with `query`, and reads the
/// reply to the close.
fn send_callback(redirect_uri: &str, query: &str) -> String {
    let (_, rest) = redirect_uri.split_once("://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .write_all(format!("GET /{path}?{query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

/// Runs `login` to its result, driving one callback with `query_for` built
/// from the authorization URL. `query_for` receives the URL `present` saw.
/// A login that fails before presenting surfaces its error instead of
/// hanging on the URL wait.
fn run_with_callback(
    server_url: &str,
    files: &FakeTokens,
    clock: &Arc<fakes::clock::FakeClock>,
    query_for: impl Fn(&str) -> String,
) -> Result<(), LoginError> {
    let (sent, received) = mpsc::channel();
    let (sent_done, received_done) = mpsc::channel();
    let clock = Arc::clone(clock);
    std::thread::scope(|scope| {
        let login = scope.spawn(|| {
            let result = login(
                files,
                server_url,
                &|url| sent.send(url.to_owned()).unwrap(),
                &*clock,
            );
            sent_done.send(result).unwrap();
        });
        let url = match fakes::Deadline::after(WITHIN).recv(&received) {
            Ok(url) => url,
            Err(_) => {
                // No authorization URL: the login failed before the
                // wait, and its result names why.
                let result = fakes::Deadline::after(WITHIN)
                    .recv_or_fail(&received_done, "the early login failure");
                login.join().unwrap();
                return result;
            }
        };
        let query = query_for(&url);
        let redirect = query_param(&url, "redirect_uri").unwrap();
        send_callback(&redirect, &query);
        login.join().unwrap();
        fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the driven login")
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
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    Response::status(404, "{}")
                }
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
            let error = login(&files, &url, &|_| panic!("no authorization URL is shown"), &*clock).unwrap_err();
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
            assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_some());
        }
    }
}

#[test]
fn a_dropped_metadata_connection_fails_discovery() {
    let server = ProviderServer::start_responding(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                Response::drop_connection()
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
            assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none());
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
        let error = login(&files, &server_url(&server), &|_| panic!("no authorization URL is shown"), &*clock).unwrap_err();
        assert!(
            matches!(error, LoginError::Registration(_)),
            "{case}: {error:?}"
        );
        assert!(!error.to_string().contains("secret-body"), "{case}: {error}");
        let requests = server.requests();
        assert_eq!(
            requests.iter().filter(|request| request.path == "/register").count(),
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
    let error = login(&files, &url, &|_| panic!("no authorization URL is shown"), &*clock)
        .unwrap_err();
    assert!(matches!(error, LoginError::Discovery(_)), "{error:?}");
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none());
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
    let stored = files.read(&crate::token::token_key(&url).unwrap()).unwrap().unwrap();
    assert_eq!(stored["token_endpoint"], json!(format!("{}/token", server.url())));
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
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_some());
}

#[test]
fn the_well_known_paths_are_tried_path_inserted_first() {
    let server = ProviderServer::start_responding(|request: &Request| {
        let base = base_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mcp") => Response::status(401, "{}"),
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                Response::status(404, "{}")
            }
            ("GET", "/.well-known/oauth-protected-resource") => {
                json_response(&json!({"authorization_servers": [base.clone()]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => {
                Response::status(404, "{}")
            }
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
    let paths: Vec<String> = server.requests().into_iter().map(|request| request.path).collect();
    assert!(paths.contains(&"/.well-known/oauth-protected-resource/mcp".to_owned()));
    assert!(paths.contains(&"/.well-known/openid-configuration".to_owned()));
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
                ("GET", "/.well-known/oauth-authorization-server") => {
                    json_response(&document)
                }
                _ => Response::status(404, "{}"),
            }
        })
        .unwrap();
        let files = FakeTokens::default();
        let clock = fakes::clock::FakeClock::new();
        let url = server_url(&server);
        let error = login(&files, &url, &|_| panic!("no authorization URL is shown"), &*clock)
            .unwrap_err();
        assert!(matches!(error, LoginError::PkceUnsupported), "{error:?}");
        assert_eq!(error.code(), ErrorCode::ConfigInvalid);
        assert!(
            server.requests().iter().all(|request| request.path != "/register"),
            "a registration request was sent"
        );
        assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none());
    }
}

#[test]
fn the_authorization_url_and_exchange_carry_the_pkce_shape() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let canonical = crate::token::canonical_url(&url).unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let login = scope.spawn(|| login(&files, &url, &|url| sent.send(url.to_owned()).unwrap(), &*clock));
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the authorization URL");
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
        assert!(redirect.ends_with("/callback"), "{redirect}");
        assert_eq!(query_param(&shown, "response_type").as_deref(), Some("code"));
        assert_eq!(query_param(&shown, "client_id").as_deref(), Some("c1"));
        assert_eq!(query_param(&shown, "code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(query_param(&shown, "resource").as_deref(), Some(canonical.as_str()));
        let challenge = query_param(&shown, "code_challenge").unwrap();
        let state = query_param(&shown, "state").unwrap();
        assert_eq!(challenge.len(), 43, "{challenge}");
        assert_eq!(state.len(), 43, "{state}");
        send_callback(&redirect, &format!("code=abc&state={state}"));
        login.join().unwrap().unwrap();
    });
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
            (super::decode(key), super::decode(value))
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
    assert_eq!(form.iter().count(), 6);
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
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let login = scope.spawn(|| login(&files, &url, &|url| sent.send(url.to_owned()).unwrap(), &*clock));
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the authorization URL");
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        let state = query_param(&shown, "state").unwrap();
        let challenge = query_param(&shown, "code_challenge").unwrap();
        send_callback(&redirect, &format!("code=abc&state={state}"));
        login.join().unwrap().unwrap();
        let exchange = server
            .requests()
            .into_iter()
            .find(|request| request.path == "/token")
            .unwrap();
        let verifier = String::from_utf8_lossy(&exchange.body)
            .split('&')
            .find_map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (key == "code_verifier").then(|| super::decode(value))
            })
            .unwrap();
        let expected = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()),
        );
        assert_eq!(challenge, expected);
    });
}

#[test]
fn the_stored_record_holds_the_token_the_endpoint_and_the_url() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    run_with_callback(&url, &files, &clock, succeed).unwrap();
    let stored = files.read(&crate::token::token_key(&url).unwrap()).unwrap().unwrap();
    let canonical = crate::token::canonical_url(&url).unwrap();
    assert_eq!(stored["access_token"], json!("at-1"));
    assert_eq!(stored["refresh_token"], json!("rt-1"));
    assert_eq!(stored["token_endpoint"], json!(format!("{}/token", server.url())));
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
    let stored = files.read(&crate::token::token_key(&url).unwrap()).unwrap().unwrap();
    assert!(stored.get("expires_at").is_none(), "{stored}");
}

#[test]
fn bad_token_replies_fail_and_store_nothing() {
    for (reply, what) in [
        (json!({"token_type": "Bearer"}).to_string(), "no access token"),
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
        assert!(matches!(error, LoginError::Exchange(_)), "{what}: {error:?}");
        assert!(!error.to_string().contains("secret"), "{what}: {error}");
        assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none(), "{what}");
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
        assert!(matches!(error, LoginError::Exchange(_)), "{case}: {error:?}");
        assert!(!error.to_string().contains("secret-body"), "{case}: {error}");
        let exchanges = server.requests().into_iter().filter(|request| request.path == "/token").count();
        assert_eq!(exchanges, 1, "{case}: the exchange retried");
        assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none(), "{case}");
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
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none());
    assert!(server.requests().iter().all(|request| request.path != "/token"));
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
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_none());
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
    assert_eq!(error.to_string(), "The authorization server returned an error.");
}

#[test]
fn a_request_for_another_path_gets_404_and_the_wait_continues() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let login = scope.spawn(|| login(&files, &url, &|url| sent.send(url.to_owned()).unwrap(), &*clock));
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the authorization URL");
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        let mut other = TcpStream::connect(host).unwrap();
        other
            .write_all(format!("GET /other HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes())
            .unwrap();
        let mut reply = String::new();
        other.read_to_string(&mut reply).unwrap();
        assert!(reply.contains("404"), "{reply}");
        let state = query_param(&shown, "state").unwrap();
        send_callback(&redirect, &format!("code=abc&state={state}"));
        login.join().unwrap().unwrap();
    });
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_some());
}

#[test]
fn a_head_past_8kib_gets_431_and_the_wait_continues() {
    let server = start_full();
    let files = FakeTokens::default();
    let clock = fakes::clock::FakeClock::new();
    let url = server_url(&server);
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let login = scope.spawn(|| login(&files, &url, &|url| sent.send(url.to_owned()).unwrap(), &*clock));
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the authorization URL");
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        // A head of exactly 8 KiB is past the cap; one byte less reads.
        // The refusal may arrive as a 431 or as a reset connection: the
        // server closed with the request half-read, and either way the
        // wait continues for the real callback below.
        let mut big = TcpStream::connect(host).unwrap();
        big.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let padding = "x".repeat(MAX_HEAD);
        let _written = big.write_all(format!("GET /callback?{padding} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes());
        let mut reply = String::new();
        match big.read_to_string(&mut reply) {
            Ok(_) => assert!(reply.contains("431"), "{reply}"),
            Err(_) => {}
        }
        let state = query_param(&shown, "state").unwrap();
        send_callback(&redirect, &format!("code=abc&state={state}"));
        login.join().unwrap().unwrap();
    });
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_some());
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
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let login = scope.spawn(|| login(&files, &url, &|url| sent.send(url.to_owned()).unwrap(), &*clock));
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received, "the authorization URL");
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        // An idle connection, opened with no bytes: the listener drops it
        // after its silent polls, which this read sees as the close.
        let mut idle = TcpStream::connect(host).unwrap();
        idle.set_read_timeout(Some(WITHIN)).unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(idle.read(&mut byte).unwrap(), 0, "the idle connection stayed open");
        let state = query_param(&shown, "state").unwrap();
        send_callback(&redirect, &format!("code=abc&state={state}"));
        login.join().unwrap().unwrap();
    });
    assert!(files.read(&crate::token::token_key(&url).unwrap()).unwrap().is_some());
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
            let result = login(files, &url, &|url| sent_url.send(url.to_owned()).unwrap(), &*moved);
            sent_done.send(result).unwrap();
        });
        // The authorization URL is shown before the wait.
        fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        assert!(
            clock.await_parked(deadline, WITHIN),
            "the login never waited on the clock"
        );
        clock.advance(CALLBACK_WAIT);
        let result =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the timed-out login");
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        login.join().unwrap();
    });
    assert!(files.read(&crate::token::token_key(&server_url(&server)).unwrap()).unwrap().is_none());
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
            let result = login(files, &url, &|url| sent_url.send(url.to_owned()).unwrap(), &*moved);
            sent_done.send(result).unwrap();
        });
        fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        assert!(
            clock.await_parked(deadline, WITHIN),
            "the login never waited on the clock"
        );
        clock.advance(Duration::from_secs(899));
        // Still waiting: a short bounded receive must time out.
        assert!(
            fakes::Deadline::after(Duration::from_millis(200)).recv(&received_done).is_err(),
            "the login ended before 900 s"
        );
        clock.advance(Duration::from_secs(1));
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
            let result = login(files, &url, &|url| sent_url.send(url.to_owned()).unwrap(), &*moved);
            sent_done.send(result).unwrap();
        });
        let shown = fakes::Deadline::after(WITHIN).recv_or_fail(&received_url, "the authorization URL");
        let deadline = clock.origin() + CALLBACK_WAIT;
        assert!(
            clock.await_parked(deadline, WITHIN),
            "the login never waited on the clock"
        );
        // A slow drip: one byte per poll, never completing a head.
        let redirect = query_param(&shown, "redirect_uri").unwrap();
        let (_, rest) = redirect.split_once("://").unwrap();
        let (host, _) = rest.split_once('/').unwrap();
        let mut drip = TcpStream::connect(host).unwrap();
        drip.write_all(b"G").unwrap();
        scope.spawn(move || {
            for _ in 0..200 {
                std::thread::sleep(Duration::from_millis(30));
                if drip.write_all(b"G").is_err() {
                    return;
                }
            }
        });
        clock.advance(CALLBACK_WAIT);
        let result =
            fakes::Deadline::after(WITHIN).recv_or_fail(&received_done, "the timed-out login");
        assert!(matches!(result, Err(LoginError::Timeout)), "{result:?}");
        login.join().unwrap();
    });
    assert!(files.read(&crate::token::token_key(&server_url(&server)).unwrap()).unwrap().is_none());
}
