//! The `openrouter` package's `cost()` (`docs/model-routing.md`, "Cost"),
//! tested as an extension: the real package, with the fake server answering
//! its `host.http` lookup (`docs/testing.md`, "Model calls").

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::Setup;
use config::Secret;
use contract::GenerationId;
use contract::provider::{ModelCall, ModelRequest, Provider};
use extensions::{Error, LuaExtension, LuaProvider};
use fakes::clock::FakeClock;
use fakes::{ProviderServer, Response, fingerprint};
use serde_json::json;

/// How long a test waits for one lookup.
const WAIT: Duration = Duration::from_secs(5);

fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the lookup did not return within {WAIT:?}"))
}

fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/openrouter")
}

/// The package's `openrouter` provider, on a fake clock.
fn openrouter(setup: &Setup) -> Arc<LuaProvider> {
    let extension = Arc::new(LuaExtension::new(
        "github.com/aakshintala/fiber/providers/openrouter",
        package(),
        setup.home(),
        FakeClock::new(),
    ));
    LuaProvider::new(extension, "openrouter")
}

fn base_url(server: &ProviderServer) -> String {
    format!("{}/api/v1", server.url())
}

/// Looks up `id` against a server answering `response`, with key `sk-test`.
fn lookup(id: &str, response: Response) -> (Result<Option<f64>, Error>, ProviderServer) {
    let setup = Setup::new();
    let server = ProviderServer::start([response]).unwrap();
    let provider = openrouter(&setup);
    let url = base_url(&server);
    let id = GenerationId(id.into());
    let result = within(move || provider.cost(&id, &url, Some(&Secret::new("sk-test".into()))));
    (result, server)
}

#[test]
fn a_200_returns_total_cost_from_the_generation_endpoint_with_the_key() {
    let (result, server) = lookup(
        "gen-abc",
        Response::status(
            200,
            json!({"data": {"id": "gen-abc", "total_cost": 0.0000072}}).to_string(),
        ),
    );
    assert_eq!(result.unwrap(), Some(0.0000072));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/v1/generation?id=gen-abc");
    assert_eq!(
        requests[0].header("authorization"),
        Some(fingerprint("Bearer sk-test").as_str())
    );
}

#[test]
fn a_404_returns_nothing() {
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(
            404,
            json!({"error": {"message": "Generation gen-abc not found", "code": 404}}).to_string(),
        ),
    );
    assert_eq!(result.unwrap(), None);
}

#[test]
fn a_200_with_a_null_total_cost_returns_nothing_and_a_zero_one_returns_zero() {
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(200, json!({"data": {"total_cost": null}}).to_string()),
    );
    assert_eq!(result.unwrap(), None);
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(200, json!({"data": {"total_cost": 0}}).to_string()),
    );
    assert_eq!(result.unwrap(), Some(0.0));
}

#[test]
fn a_200_with_a_cost_that_is_not_a_number_returns_nothing() {
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(200, json!({"data": {"total_cost": "0.1"}}).to_string()),
    );
    assert_eq!(result.unwrap(), None);
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(200, json!({"data": null}).to_string()),
    );
    assert_eq!(result.unwrap(), None);
}

#[test]
fn a_500_returns_nothing() {
    let (result, _server) = lookup(
        "gen-abc",
        Response::status(500, json!({"data": {"total_cost": 1}}).to_string()),
    );
    assert_eq!(result.unwrap(), None);
}

/// A provider that is never called: the lookup is all these tests use.
struct Unused;

impl Provider for Unused {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        panic!("Unused::call is not used")
    }
}

/// Looks up `gen-abc` through the cost lookup the package gives a provider.
fn through_lookup(response: Response) -> Option<f64> {
    let setup = Setup::new();
    let server = ProviderServer::start([response]).unwrap();
    let provider = openrouter(&setup);
    let url = base_url(&server);
    within(move || {
        let costed = provider
            .costed(Arc::new(Unused), &url, Some(Secret::new("sk-test".into())))
            .unwrap();
        costed
            .cost_lookup()
            .unwrap()
            .cost(&GenerationId("gen-abc".into()))
    })
}

#[test]
fn a_body_that_is_not_json_raises_and_is_nothing_to_the_lookup() {
    let (result, _server) = lookup("gen-abc", Response::status(200, "<html>busy</html>"));
    assert!(matches!(result, Err(Error::Lua { .. })), "{result:?}");
    assert_eq!(
        through_lookup(Response::status(200, "<html>busy</html>")),
        None
    );
}

#[test]
fn a_dropped_connection_is_nothing_to_the_lookup() {
    assert_eq!(through_lookup(Response::drop_connection()), None);
}

#[test]
fn the_id_is_percent_encoded() {
    let (result, server) = lookup(
        "gen&a b",
        Response::status(200, json!({"data": {"total_cost": 1}}).to_string()),
    );
    assert_eq!(result.unwrap(), Some(1.0));
    assert_eq!(
        server.requests()[0].path,
        "/api/v1/generation?id=gen%26a%20b"
    );
}

#[test]
fn without_a_key_no_authorization_is_sent() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(
        200,
        json!({"data": {"total_cost": 1}}).to_string(),
    )])
    .unwrap();
    let provider = openrouter(&setup);
    let url = base_url(&server);
    let result = within(move || provider.cost(&GenerationId("gen-abc".into()), &url, None));
    assert_eq!(result.unwrap(), Some(1.0));
    assert_eq!(server.requests()[0].header("authorization"), None);
}
