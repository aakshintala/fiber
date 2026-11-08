//! `connect` fetches quota for the credential label it is given
//! (`docs/tools.md`, "Provider quota").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use contract::provider::ModelRequest;
use contract::provider::Provider;
use fakes::Deadline;
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
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        media: Default::default(),
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
        databricks_request_tags: None,
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
    let provider = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&connected)
        .expect("connect ended in time")
        .unwrap();
    let _call = provider.call(&request());
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
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
        databricks_request_tags: None,
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
        databricks_request_tags: None,
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

/// The request-tags header as the fake server records it (names are
/// lowercased there).
const TAGS_HEADER: &str = "databricks-ai-gateway-request-tags";

/// Provider `name` with one model on `protocol` at `base_url`, declaring the
/// source-only tags header under `tags_name` and one unrelated header.
fn tagged(name: &str, protocol: &str, base_url: &str, tags_name: &str) -> config::ProviderData {
    serde_json::from_value(json!({
        "name": name,
        "headers": {
            tags_name: "{\"source\":\"fiber\"}",
            "X-Other": "keep",
        },
        "models": [{
            "id": "m",
            "protocol": protocol,
            "base_url": base_url,
        }],
    }))
    .unwrap()
}

fn here_with(tags: Option<&[(&str, &str)]>) -> crate::Here {
    crate::Here {
        workspace: std::path::PathBuf::new(),
        clock: fakes::clock::FakeClock::new(),
        cache_diagnostics: false,
        databricks_request_tags: tags.map(|pairs| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        }),
    }
}

/// Runs `call` to its end under a deadline, so a hung call fails the test.
fn finish(call: Box<dyn contract::provider::ModelCall>) {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(done.send(call.run(&mut |_| {})));
    });
    // The fake answers an error status; the call's outcome is not the
    // subject, only that it ended.
    drop(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&finished)
            .expect("the call ended in time"),
    );
}

/// Which way a request is made.
#[derive(Clone, Copy)]
enum Way {
    Plain,
    Observed,
}

/// The request one model call sends to a fake server, through `connect` or
/// the observed factory `connect_observed` builds.
fn sent(
    way: Way,
    name: &str,
    protocol: &str,
    tags_name: &str,
    tags: Option<&[(&str, &str)]>,
) -> fakes::Request {
    let server = fakes::ProviderServer::start([fakes::Response::status(400, "{}")]).unwrap();
    let data = tagged(name, protocol, &format!("{}/v1", server.url()), tags_name);
    let model = extensions::Model {
        provider: &data,
        model: &data.models[0],
        thinking: None,
    };
    let key = Some(contract::Secret::new("k".to_owned()));
    let here = here_with(tags);
    match way {
        Way::Plain => {
            let provider = crate::connect(model, key, None, None, &here).unwrap();
            finish(provider.call(&request()));
        }
        Way::Observed => {
            let diag = crate::Diagnostics {
                log: Arc::new(crate::test_support::Level { debug: true }),
                timings: Arc::new(|_, _, _| None),
            };
            let (_provider, observe) =
                crate::connect_observed(model, key, None, None, &here, &diag).unwrap();
            let observe = observe.expect("a factory at debug");
            finish(observe(&request(), std::num::NonZeroU32::MIN));
        }
    }
    assert!(
        server.await_requests(1, Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the model call never reached the server"
    );
    server.requests().into_iter().next().unwrap()
}

/// The one tags header of `request`, parsed.
fn parsed_tags(request: &fakes::Request) -> serde_json::Value {
    let all: Vec<&str> = request
        .headers
        .iter()
        .filter(|(name, _)| name == TAGS_HEADER)
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        all.len(),
        1,
        "exactly one tags header: {:?}",
        request.headers
    );
    let raw = all.first().unwrap();
    assert!(
        !raw.contains(['\n', '\r']),
        "no raw line break in the header: {raw:?}"
    );
    serde_json::from_str(raw).unwrap()
}

const PROTOCOLS: [&str; 3] = [
    "anthropic-messages",
    "openai-responses",
    "openai-completions",
];
const NAME: &str = "Databricks-Ai-Gateway-Request-Tags";
/// A tag with a quote, a backslash, a newline and a non-ASCII character.
const AWKWARD: &str = "a\"b\\c\nd é";

#[test]
fn request_tags_merge_for_plain_and_observed_connections() {
    for way in [Way::Plain, Way::Observed] {
        for protocol in PROTOCOLS {
            let with = |tags| sent(way, "databricks", protocol, NAME, tags);
            // No tags: the declared source-only header stands.
            assert_eq!(parsed_tags(&with(None)), json!({"source": "fiber"}));
            // An empty object sends the same.
            assert_eq!(parsed_tags(&with(Some(&[]))), json!({"source": "fiber"}));
            // Tags merge beside the source.
            assert_eq!(
                parsed_tags(&with(Some(&[("run", "r1")]))),
                json!({"source": "fiber", "run": "r1"})
            );
            // Special characters survive the wire, escaped in the header.
            assert_eq!(
                parsed_tags(&with(Some(&[("run", AWKWARD)]))),
                json!({"source": "fiber", "run": AWKWARD})
            );
        }
    }
}

#[test]
fn request_tags_keep_source_and_other_headers() {
    for way in [Way::Plain, Way::Observed] {
        for protocol in PROTOCOLS {
            let baseline = sent(way, "databricks", protocol, NAME, None);
            // The declared name is mixed-case and differs from the canonical
            // one: it is replaced, not duplicated.
            for declared in [NAME, "databricks-AI-gateway-request-TAGS"] {
                let request = sent(
                    way,
                    "databricks",
                    protocol,
                    declared,
                    Some(&[("source", "other"), ("run", "r1")]),
                );
                assert_eq!(
                    parsed_tags(&request),
                    json!({"source": "fiber", "run": "r1"})
                );
                // Every other header, credentials included, is unchanged.
                let rest = |r: &fakes::Request| {
                    r.headers
                        .iter()
                        .filter(|(name, _)| name != TAGS_HEADER && name != "host")
                        .cloned()
                        .collect::<Vec<_>>()
                };
                assert_eq!(rest(&request), rest(&baseline));
                assert_eq!(request.header("x-other"), Some("keep"));
            }
        }
    }
}

#[test]
fn request_tags_leave_other_providers_unchanged() {
    for way in [Way::Plain, Way::Observed] {
        for protocol in PROTOCOLS {
            let request = sent(way, "other", protocol, NAME, Some(&[("run", "r1")]));
            assert_eq!(parsed_tags(&request), json!({"source": "fiber"}));
            // A provider that declares no such header gains none.
            let request = sent(way, "other", protocol, "X-Declared", Some(&[("run", "r1")]));
            assert_eq!(request.header(TAGS_HEADER), None);
        }
    }
}
