//! A model call through a session held open through the hub starts the
//! Lua provider's quota fetch off the request path (`docs/tools.md`,
//! "Provider quota"): the fake provider server records a `/lua-quota`
//! request while the session stays open. `fiber ask` exits without
//! waiting for that fetch, so no `ask` test asserts it (see #2593).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::json;
use support::*;

/// Installs the Lua extension `test-ext` with a `test` provider whose
/// models, credential and quota functions each make one real HTTP request.
/// Model URLs come from the test server, so no secret is configured
/// anywhere.
fn install_test_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("luasrc");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "test-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"test": [format!("{server_url}/v1")]}}),
    );
    fs::create_dir_all(source.join("providers")).unwrap();
    fs::write(
        source.join("providers/test.lua"),
        format!(
            "fiber.provider(\"test\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ url = \"{server_url}/lua-models\" }})\n\
                 return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{server_url}/v1\", context_window = 100000 }}}}\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 local r = host.http({{ url = \"{server_url}/lua-quota\" }})\n\
                 return {{ windows = {{ {{ name = \"w\", used_percent = 1 }} }} }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// A server answering every Lua purpose plus native model calls, without
/// limit.
fn start_routed() -> ProviderServer {
    ProviderServer::start_responding(|request| match request.path.as_str() {
        "/lua-models" | "/lua-token" | "/lua-quota" => Response::status(200, "{}"),
        "/v1/responses" => hello(),
        _ => Response::status(500, "unrouted"),
    })
    .unwrap()
}

fn configure(setup: &Setup, model: &str) {
    write_json(&setup.home().join("config.json"), &json!({"model": model}));
}

#[test]
fn a_model_call_through_an_open_session_fetches_lua_quota() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    configure(&setup, "test/m1");
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "hi");
    subscribe(&client, &session);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    // The quota fetch runs off the request path, so it can land after the
    // turn does: each wait below takes what remains of the test's one
    // deadline, and expiry fails naming the missing request.
    let mut seen = server.requests().len();
    loop {
        if server
            .requests()
            .iter()
            .any(|request| request.path == "/lua-quota")
        {
            break;
        }
        assert!(
            server.await_requests(seen + 1, setup.deadline.left()),
            "waited until the deadline for the /lua-quota request after the turn"
        );
        seen = server.requests().len();
    }
    drop(client);
    // SIGKILL the hub: the session keeps running on its own socket.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
}
