//! `connect` fetches quota for the credential label it is given
//! (`docs/tools.md`, "Provider quota").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::Arc;

use contract::provider::ModelRequest;
use serde_json::json;

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
    let here = crate::connect::Here {
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
        drop(done.send(crate::connect::connect(
            model,
            Some(contract::Secret::new("k".to_owned())),
            None,
            Some((&lua, "work")),
            &here,
        )));
    });
    let provider = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&connected)
        .expect("connect ended in time")
        .unwrap();
    let _call = provider.call(&request());
    assert!(
        server.await_requests(1, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the model call never fetched quota"
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(paths, ["/work/k"]);
}
