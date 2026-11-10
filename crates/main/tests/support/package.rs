//! A first-party package copied with its origin rewritten to the fake
//! server, and the OpenRouter listing its tests serve.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

use super::{Setup, write_json};

/// Copies the first-party package `providers/<name>` to `to` with every
/// base URL's origin `origin` replaced by `url`, and returns `to`.
pub(crate) fn copy_package(name: &str, to: &Path, origin: &str, url: &str) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    let mut files = vec!["extension.json".to_owned()];
    for entry in fs::read_dir(from.join("providers")).unwrap() {
        let file = entry.unwrap().file_name();
        files.push(format!("providers/{}", file.to_str().unwrap()));
    }
    // A Lua package's entry script, such as `openrouter`'s `models()`.
    for file in files {
        let text = fs::read_to_string(from.join(&file)).unwrap();
        fs::create_dir_all(to.join(&file).parent().unwrap()).unwrap();
        fs::write(to.join(&file), text.replace(origin, url)).unwrap();
    }
    to.to_path_buf()
}

/// The recorded OpenRouter listing `models()` serves in the `ask` tests:
/// the reply body of `models-from-the-recording.json`'s scripted request.
pub(crate) fn openrouter_listing() -> Vec<u8> {
    let case: Value = serde_json::from_str(include_str!(
        "../../../../providers/openrouter/tests/models-from-the-recording.json"
    ))
    .expect("the recording case reads");
    case["host"]["http"][0]["reply"]["body"]
        .as_str()
        .expect("the recording case holds one body")
        .as_bytes()
        .to_vec()
}

/// The recorded OpenRouter quota reply `quota()` serves in the `ask` tests:
/// the reply body of `quota-limited-key-returns-credit-and-limit.json`'s
/// scripted request.
pub(crate) fn openrouter_quota() -> Vec<u8> {
    let case: Value = serde_json::from_str(include_str!(
        "../../../../providers/openrouter/tests/quota-limited-key-returns-credit-and-limit.json"
    ))
    .expect("the quota case reads");
    case["host"]["http"][0]["reply"]["body"]
        .as_str()
        .expect("the quota case holds one body")
        .as_bytes()
        .to_vec()
}

/// The recorded Codex usage reply `quota()` serves in the `codex` tests:
/// the reply body of `quota-two-windows.json`'s scripted request.
pub(crate) fn codex_quota() -> Vec<u8> {
    let case: Value = serde_json::from_str(include_str!(
        "../../../../providers/codex/tests/quota-two-windows.json"
    ))
    .expect("the quota case reads");
    case["host"]["http"][0]["reply"]["body"]
        .as_str()
        .expect("the quota case holds one body")
        .as_bytes()
        .to_vec()
}

/// The recorded opencode-go usage reply `quota()` serves in the `ask`
/// tests: the reply body of
/// `quota-recorded-usage-returns-three-windows.json`'s scripted request.
pub(crate) fn opencode_quota() -> Vec<u8> {
    let case: Value = serde_json::from_str(include_str!(
        "../../../../providers/opencode/tests/quota-recorded-usage-returns-three-windows.json"
    ))
    .expect("the quota case reads");
    case["host"]["http"][0]["reply"]["body"]
        .as_str()
        .expect("the quota case holds one body")
        .as_bytes()
        .to_vec()
}

/// Renders header pairs as a Lua table literal, such as
/// `{ ["chatgpt-account-id"] = "acc" }`.
fn lua_headers(pairs: &[(&str, &str)]) -> String {
    let mut table = "{".to_owned();
    for (name, value) in pairs {
        table.push_str(&format!(" [{name:?}] = {value:?},"));
    }
    table.push('}');
    table
}

/// Installs the Lua extension `test-ext` with a `test` provider whose
/// models, credential, quota and sign functions each answer from the fake
/// server: models serve `m1`, credential returns the subscription token
/// plus `credential_headers`, sign returns `sign_headers`, and quota
/// reports no windows. A static data file lists `m1` so the registry
/// resolves without running Lua, and a stored `default` label stands in
/// for the key the unsigned path reads.
pub(crate) fn install_test_provider_with_headers(
    setup: &Setup,
    server_url: &str,
    credential_headers: &[(&str, &str)],
    sign_headers: &[(&str, &str)],
) {
    let source = setup.root.path().join("luasrc");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "test-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"test": [format!("{server_url}/v1")]}}),
    );
    fs::create_dir_all(source.join("providers")).unwrap();
    let credential_headers = lua_headers(credential_headers);
    let sign_headers = lua_headers(sign_headers);
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
                 return {{ token = \"tok-pin-test\", expires_at = 1893456000, headers = {credential_headers} }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 return {{ windows = {{}} }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {sign_headers} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
    write_json(
        &source.join("providers/test.json"),
        &json!({"name": "test", "models": [{"id": "m1", "protocol": "openai-responses",
                "base_url": format!("{server_url}/v1"), "context_window": 100000}]}),
    );
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
    config::store_credential(
        &setup.home(),
        "test",
        "default",
        &config::Secret::new("k-test".into()),
    )
    .unwrap();
}

/// A fake opencode-go server answering by path, for the `ask` tests: the
/// background `quota()` fetch races the turn's own requests, so serving
/// by order would hand the quota request a queued reply meant for another
/// path. `/zen/go/v1/usage` always answers the recorded quota reply, and
/// `/zen/go/v1/responses` answers `responses` in order, counting the hits
/// in the closure. Anything else gets the fake's scripted-response 500,
/// as an unrouted request would.
pub(crate) fn opencode_go_ask_server(responses: Vec<Response>) -> ProviderServer {
    let quota = opencode_quota();
    let responses = Arc::new(responses);
    let next = Arc::new(AtomicUsize::new(0));
    ProviderServer::start_responding(move |request| {
        let target = request.path.split('?').next().unwrap_or_default();
        match target {
            "/zen/go/v1/usage" => Response::status(200, quota.clone()),
            "/zen/go/v1/responses" => {
                let index = next.fetch_add(1, Ordering::SeqCst);
                responses.get(index).cloned().unwrap_or_else(|| {
                    Response::status(
                        500,
                        r#"{"error":"fakes: no scripted response left for this request"}"#,
                    )
                })
            }
            _ => Response::status(
                500,
                r#"{"error":"fakes: no scripted response left for this request"}"#,
            ),
        }
    })
    .unwrap()
}

/// A fake Codex server answering by path, for the `codex` tests: the
/// background `quota()` fetch races the turn's own requests, so serving
/// by order would hand the quota request a queued reply meant for another
/// path. `/backend-api/wham/usage` always answers the recorded quota
/// reply, and `/backend-api/codex/responses` answers `responses` in
/// order, counting the hits in the closure. Anything else gets the
/// fake's scripted-response 500, as an unrouted request would.
pub(crate) fn codex_ask_server(responses: Vec<Response>) -> ProviderServer {
    let quota = codex_quota();
    let responses = Arc::new(responses);
    let next = Arc::new(AtomicUsize::new(0));
    ProviderServer::start_responding(move |request| {
        let target = request.path.split('?').next().unwrap_or_default();
        match target {
            "/backend-api/wham/usage" => Response::status(200, quota.clone()),
            "/backend-api/codex/responses" => {
                let index = next.fetch_add(1, Ordering::SeqCst);
                responses.get(index).cloned().unwrap_or_else(|| {
                    Response::status(
                        500,
                        r#"{"error":"fakes: no scripted response left for this request"}"#,
                    )
                })
            }
            _ => Response::status(
                500,
                r#"{"error":"fakes: no scripted response left for this request"}"#,
            ),
        }
    })
    .unwrap()
}

/// A fake OpenRouter server answering by path, for the `ask` tests: the
/// background `quota()` fetch races the turn's own requests, so serving
/// by order would hand the quota request a queued reply meant for another
/// path. `/api/v1/key` always answers the recorded quota reply,
/// `/api/v1/models` the recorded listing, and
/// `/api/v1/chat/completions` answers `completions` in order, counting
/// the hits in the closure (the retry test hits it twice). Anything else
/// gets the fake's scripted-response 500, as an unrouted request would.
pub(crate) fn openrouter_ask_server(completions: Vec<Response>) -> ProviderServer {
    let listing = openrouter_listing();
    let quota = openrouter_quota();
    let completions = Arc::new(completions);
    let next = Arc::new(AtomicUsize::new(0));
    ProviderServer::start_responding(move |request| {
        let target = request.path.split('?').next().unwrap_or_default();
        match target {
            "/api/v1/key" => Response::status(200, quota.clone()),
            "/api/v1/models" => Response::status(200, listing.clone()),
            "/api/v1/chat/completions" => {
                let index = next.fetch_add(1, Ordering::SeqCst);
                completions.get(index).cloned().unwrap_or_else(|| {
                    Response::status(
                        500,
                        r#"{"error":"fakes: no scripted response left for this request"}"#,
                    )
                })
            }
            _ => Response::status(
                500,
                r#"{"error":"fakes: no scripted response left for this request"}"#,
            ),
        }
    })
    .unwrap()
}
