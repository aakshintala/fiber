//! `connect` fetches quota for the credential label it is given
//! (`docs/tools.md`, "Provider quota").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;
use std::time::Duration;

use contract::provider::ModelRequest;
use contract::provider::Provider;
use serde_json::json;

/// How long a test waits for the quota fetch.
const WAIT: Duration = Duration::from_secs(5);

/// A Lua provider `qp` whose `quota()` asks `<server>/<label>/<key>` over
/// HTTP.
fn quota_lua(root: &fakes::TempDir, server: &str) -> Arc<extensions::LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(dir.join("providers")).unwrap();
    std::fs::write(
        dir.join("providers/qp.lua"),
        format!(
            "fiber.provider(\"qp\", {{ quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{server}/\" .. a.label .. \"/\" .. tostring(a.key) }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }} }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(extensions::LuaExtension::new(
        "ext",
        dir,
        home,
        fakes::clock::FakeClock::new(),
    ));
    extensions::LuaProvider::new(extension, "qp")
}

fn request() -> ModelRequest {
    ModelRequest {
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

/// A model call through `connect` fetches quota for the label `connect` was
/// given, not for any other account.
#[test]
fn connect_fetches_quota_for_its_label() {
    let root = fakes::TempDir::new("fiber-connect-quota");
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, "{\"windows\": []}")]).unwrap();
    let lua = quota_lua(&root, &server.url());
    let data: config::ProviderData = serde_json::from_value(json!({
        "name": "qp",
        "models": [{
            "id": "m",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:9/v1",
        }],
    }))
    .unwrap();
    let here = crate::Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
    };
    // `connect` can wait on Lua registration, so it runs on its own thread
    // under a deadline instead of hanging the test.
    let (done, connected) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let model = extensions::Model {
            provider: &data,
            model: &data.models[0],
            thinking: None,
        };
        drop(done.send(crate::connect(
            model,
            Some(contract::Secret::new("k".to_owned())),
            None,
            Some((&lua, "work")),
            &here,
        )));
    });
    let provider = fakes::Deadline::after(WAIT)
        .recv(&connected)
        .expect("connect ended in time")
        .unwrap();
    let _call = provider.call(&request());
    assert!(
        server.await_requests(1, fakes::Deadline::after(WAIT)),
        "the model call never fetched quota"
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(paths, ["/work/k"]);
}

/// `connect` over provider data naming `deferred_tools`.
fn provider_for(protocol: &str, deferred_tools: bool) -> Arc<dyn Provider> {
    let data: config::ProviderData = serde_json::from_value(json!({
        "name": "qp",
        "models": [{
            "id": "m",
            "protocol": protocol,
            "base_url": "http://127.0.0.1:9/v1",
            "deferred_tools": deferred_tools,
        }],
    }))
    .unwrap();
    let here = crate::Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
    };
    let model = extensions::Model {
        provider: &data,
        model: &data.models[0],
        thinking: None,
    };
    crate::connect(model, None, None, None, &here).unwrap()
}

#[test]
fn connect_passes_deferred_tools_to_responses_and_messages() {
    assert!(provider_for("openai-responses", true).defers());
    assert!(!provider_for("openai-responses", false).defers());
    assert!(provider_for("anthropic-messages", true).defers());
    assert!(!provider_for("anthropic-messages", false).defers());
}

#[test]
fn connect_leaves_a_protocol_without_deferral_in_full() {
    assert!(!provider_for("openai-completions", true).defers());
}

/// Provider data with one model on `protocol`.
fn native(protocol: &str) -> config::ProviderData {
    serde_json::from_value(json!({
        "name": "qp",
        "models": [{
            "id": "m",
            "protocol": protocol,
            "base_url": "http://127.0.0.1:9/v1",
        }],
    }))
    .unwrap()
}

/// Whether `connect_observed` builds a factory for `data`'s first model
/// (a `scripted` one reads its id as a script under `workspace`) when the
/// writer is at debug (`debug`) or at info.
fn observed(data: &config::ProviderData, workspace: &std::path::Path, debug: bool) -> bool {
    let here = crate::Here {
        workspace: workspace.to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
    };
    let diag = crate::Diagnostics {
        log: Arc::new(crate::test_support::Level { debug }),
        timings: Arc::new(|_, _, _| None),
    };
    let model = extensions::Model {
        provider: data,
        model: &data.models[0],
        thinking: None,
    };
    let (_provider, observe) =
        crate::connect_observed(model, None, None, None, &here, &diag).unwrap();
    observe.is_some()
}

#[test]
fn connect_observed_builds_a_factory_for_a_native_model_at_debug() {
    let root = fakes::TempDir::new("fiber-connect-observed");
    assert!(observed(&native("openai-responses"), root.path(), true));
    assert!(observed(&native("anthropic-messages"), root.path(), true));
}

#[test]
fn connect_observed_builds_no_factory_at_info() {
    let root = fakes::TempDir::new("fiber-connect-observed");
    assert!(!observed(&native("openai-responses"), root.path(), false));
}

#[test]
fn connect_observed_builds_no_factory_for_a_scripted_model_at_debug() {
    let root = fakes::TempDir::new("fiber-connect-observed");
    std::fs::write(root.path().join("s.json"), r#"{"steps": []}"#).unwrap();
    let mut providers = extensions::Providers::default();
    providers.add_scripted("scripted/s.json");
    let data = providers.get("scripted").unwrap().clone();
    assert!(!observed(&data, root.path(), true));
}
