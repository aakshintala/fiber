//! The `mcp_resources` tool: argument parsing, page and content mapping,
//! effects and refusals, with no child process unless a test starts one.

use std::sync::Arc;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use serde_json::{Map, Value, json};

use super::{Request, contents, page, request, servers_text};
use crate::error::McpError;
use crate::start::{DEFAULT_CALL_TIMEOUT, ServerSpec};
use crate::test_support::{FakeImages, Setup, WITHIN};
use fakes::Deadline;

fn args(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

fn cancel() -> fakes::CancelToken {
    fakes::CancelToken::new()
}

fn emit() -> fakes::Recorder {
    fakes::Recorder::default()
}

#[test]
fn request_reads_each_action() {
    let rows: &[(&str, Value, Result<Request, McpError>)] = &[
        (
            "list without a server names the servers",
            json!({"action": "list"}),
            Ok(Request::Servers),
        ),
        (
            "list with a server lists one page",
            json!({"action": "list", "server": "docs"}),
            Ok(Request::List {
                server: "docs".to_owned(),
                cursor: None,
            }),
        ),
        (
            "list with a cursor continues the page",
            json!({"action": "list", "server": "docs", "cursor": "p2"}),
            Ok(Request::List {
                server: "docs".to_owned(),
                cursor: Some("p2".to_owned()),
            }),
        ),
        (
            "read names its server and uri",
            json!({"action": "read", "server": "docs", "uri": "file:///notes.txt"}),
            Ok(Request::Read {
                server: "docs".to_owned(),
                uri: "file:///notes.txt".to_owned(),
            }),
        ),
        (
            "read ignores a cursor",
            json!({"action": "read", "server": "docs", "uri": "u", "cursor": "p2"}),
            Ok(Request::Read {
                server: "docs".to_owned(),
                uri: "u".to_owned(),
            }),
        ),
        (
            "list without a server ignores a cursor and a uri",
            json!({"action": "list", "cursor": "p2", "uri": "u"}),
            Ok(Request::Servers),
        ),
        (
            "list with a server ignores a uri",
            json!({"action": "list", "server": "docs", "uri": "u"}),
            Ok(Request::List {
                server: "docs".to_owned(),
                cursor: None,
            }),
        ),
        (
            "read without a uri fails",
            json!({"action": "read", "server": "docs"}),
            Err(McpError::ResourceReadNeedsServerAndUri),
        ),
        (
            "read without a server fails",
            json!({"action": "read", "uri": "u"}),
            Err(McpError::ResourceReadNeedsServerAndUri),
        ),
        (
            "another action fails",
            json!({"action": "write"}),
            Err(McpError::ResourceActionUnknown),
        ),
        (
            "no action fails",
            json!({}),
            Err(McpError::ResourceActionUnknown),
        ),
        (
            "a non-string action fails",
            json!({"action": 7}),
            Err(McpError::ResourceActionUnknown),
        ),
        (
            "a non-string server fails",
            json!({"action": "list", "server": 5}),
            Err(McpError::ResourceFieldNotString { field: "server" }),
        ),
        (
            "a non-string uri fails",
            json!({"action": "read", "server": "docs", "uri": 5}),
            Err(McpError::ResourceFieldNotString { field: "uri" }),
        ),
        (
            "a non-string cursor fails",
            json!({"action": "list", "server": "docs", "cursor": 5}),
            Err(McpError::ResourceFieldNotString { field: "cursor" }),
        ),
    ];
    for (name, arguments, expected) in rows {
        let parsed = request(&args(arguments.clone()));
        match expected {
            Ok(expected) => assert_eq!(&parsed.expect("parsed"), expected, "row: {name}"),
            Err(expected) => assert_eq!(&parsed.expect_err("rejected"), expected, "row: {name}"),
        }
    }
}

#[test]
fn page_lists_resources_and_passes_on_the_cursor() {
    let first = page(
        "docs",
        json!({
            "resources": [
                {"uri": "file:///notes.txt", "name": "notes", "mimeType": "text/plain"},
                {"uri": "file:///logo.png", "name": "logo", "mimeType": "image/png", "size": 41},
            ],
            "nextCursor": "p2",
        }),
    )
    .expect("a page reads");
    assert_eq!(
        first,
        "{\"resources\":[\
         {\"uri\":\"file:///notes.txt\",\"name\":\"notes\",\"mimeType\":\"text/plain\"},\
         {\"uri\":\"file:///logo.png\",\"name\":\"logo\",\"mimeType\":\"image/png\",\"size\":41}\
         ],\"nextCursor\":\"p2\"}",
    );
    let last = page(
        "docs",
        json!({"resources": [{"uri": "file:///last.txt", "name": "last"}]}),
    )
    .expect("a page without a cursor reads");
    assert_eq!(
        last,
        "{\"resources\":[{\"uri\":\"file:///last.txt\",\"name\":\"last\"}]}",
    );
    // A `nextCursor` that is not a string is left out, and entries
    // without a string `uri` are dropped.
    let lenient = page(
        "docs",
        json!({
            "resources": [
                {"uri": "file:///kept.txt"},
                {"name": "no uri"},
                {"uri": 7},
            ],
            "nextCursor": 7,
        }),
    )
    .expect("a lenient page reads");
    assert_eq!(lenient, "{\"resources\":[{\"uri\":\"file:///kept.txt\"}]}");
}

#[test]
fn a_page_that_is_not_a_list_is_a_tool_error() {
    for (name, value) in [
        ("not an object", json!([{"uri": "file:///x"}])),
        ("no resources", json!({"nextCursor": "p2"})),
        (
            "resources is an object",
            json!({"resources": {"uri": "file:///x"}}),
        ),
    ] {
        assert_eq!(
            page("docs", value),
            Err(McpError::ResourceListMissing {
                server: "docs".to_owned(),
            }),
            "row: {name}",
        );
    }
}

#[test]
fn contents_map_as_embedded_resources() {
    use base64::Engine as _;
    let dir = fakes::TempDir::new("mcp-resources-contents");
    let fake = FakeImages::ok();
    let concrete = Arc::clone(&fake);
    let images: Arc<dyn contract::images::Images> = concrete;
    let call = || {
        crate::tool::Call::new(
            "docs",
            "resources/read",
            crate::tool::Target {
                timeout: DEFAULT_CALL_TIMEOUT,
                artifacts: dir.path().to_path_buf(),
                images: Arc::clone(&images),
                slot: std::sync::Weak::new(),
            },
        )
    };
    // Text gives a text part.
    let text = contents(json!({"contents": [
        {"uri": "file:///notes.txt", "mimeType": "text/plain", "text": "hello"},
    ]}))
    .expect("text reads");
    let out = crate::tool::answer(&call(), &text, &cancel());
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        vec![ContentPart::Text {
            text: "hello".to_owned()
        }]
    );
    // A non-image blob gives a saved line.
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"hi");
    let blob = contents(json!({"contents": [
        {"uri": "file:///notes.bin", "mimeType": "application/octet-stream", "blob": encoded},
    ]}))
    .expect("a blob reads");
    let out = crate::tool::answer(&call(), &blob, &cancel());
    assert!(out.error.is_none());
    assert_eq!(out.content.len(), 1);
    let ContentPart::Text { text } = &out.content[0] else {
        panic!("one saved line");
    };
    assert!(
        text.starts_with("Saved application/octet-stream content, 2 bytes, to artifacts/mcp-"),
        "line: {text}",
    );
    // An `image/png` blob goes through the image child once.
    let image_bytes = b"raw-image".to_vec();
    let image_encoded = base64::engine::general_purpose::STANDARD.encode(&image_bytes);
    let image = contents(json!({"contents": [
        {"uri": "file:///logo.png", "mimeType": "image/png", "blob": image_encoded},
    ]}))
    .expect("an image reads");
    let out = crate::tool::answer(&call(), &image, &cancel());
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        vec![
            ContentPart::Text {
                text: "Image: 2x3 image/png.\n".to_owned(),
            },
            ContentPart::Image {
                path: "artifacts/mcp-fake.png".to_owned(),
                mime_type: "image/png".to_owned(),
                width: 2,
                height: 3,
            },
        ]
    );
    assert_eq!(fake.calls(), vec![image_bytes]);
    // A non-object entry gives "An unreadable resource.".
    let unreadable =
        contents(json!({"contents": [{"uri": "file:///a.txt", "text": "a"}, 7, null]}))
            .expect("unreadable entries read");
    let out = crate::tool::answer(&call(), &unreadable, &cancel());
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        vec![
            ContentPart::Text {
                text: "a".to_owned()
            },
            ContentPart::Text {
                text: "An unreadable resource.".to_owned()
            },
            ContentPart::Text {
                text: "An unreadable resource.".to_owned()
            },
        ]
    );
    // An empty `contents` gives empty content.
    let empty = contents(json!({"contents": []})).expect("empty reads");
    let out = crate::tool::answer(&call(), &empty, &cancel());
    assert!(out.error.is_none());
    assert!(out.content.is_empty());
}

#[test]
fn contents_without_an_array_are_none() {
    for (name, value) in [
        ("not an object", json!([{"text": "hi"}])),
        ("no contents", json!({"nextCursor": "p2"})),
        ("contents is an object", json!({"contents": {"text": "hi"}})),
    ] {
        assert!(contents(value).is_none(), "row: {name}");
    }
}

/// One lazy spec for `name`, cached with `resources`, spawning nothing.
fn lazy_spec(setup: &Setup, name: &str, resources: bool) -> ServerSpec {
    let spec = setup.spec(name);
    crate::cache::write(
        &setup.cache(),
        name,
        &spec.key(),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources,
        },
    );
    spec
}

/// One `mcp_resources` reaching `started`'s slots.
fn resources(
    started: &crate::start::Started,
    artifacts: &std::path::Path,
    images: &Arc<dyn contract::images::Images>,
) -> Arc<super::Resources> {
    Arc::new(super::Resources::new(
        &started.servers.slots,
        artifacts,
        images,
    ))
}

#[test]
fn servers_text_names_offering_servers_sorted() {
    let setup = Setup::new();
    let started = setup.start(vec![
        lazy_spec(&setup, "zeta", true),
        lazy_spec(&setup, "mid", false),
        lazy_spec(&setup, "alpha", true),
    ]);
    let slots = started.servers.slots.lock().unwrap().clone();
    assert_eq!(servers_text(&slots), "{\"servers\":[\"alpha\",\"zeta\"]}");
    assert_eq!(servers_text(&[]), "{\"servers\":[]}");
    setup.stop(started.servers);
}

#[test]
fn the_definition_is_fixed_and_names_no_server() {
    let setup = Setup::new();
    let started = setup.start(vec![lazy_spec(&setup, "zeta", true)]);
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    let definition = tool.definition();
    assert_eq!(definition, super::resources_definition());
    assert_eq!(definition.name, "mcp_resources");
    assert_eq!(
        definition.description,
        "Lists and reads the resources of the MCP servers that offer them. `list` with no \
         `server` names those servers. `list` with a `server` lists its resources; when the \
         answer has a `nextCursor`, `list` again with it as `cursor` for the next page. \
         `read` reads the resource at `uri` from `server`, including one a resource link names.",
    );
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "read"], "description": "`list` or `read`."},
                "server": {"type": "string", "description": "The MCP server's configured name."},
                "uri": {"type": "string", "description": "The resource's URI, for `read`."},
                "cursor": {"type": "string", "description": "The `nextCursor` of the previous `list` answer."},
            },
            "required": ["action"],
        }),
    );
    assert!(!definition.deferred);
    assert_eq!(definition.hosted, None);
    let text = serde_json::to_string(&definition).expect("a definition writes");
    assert!(
        !text.contains("zeta"),
        "the definition names no server: {text}",
    );
    setup.stop(started.servers);
}

#[test]
fn effects_add_network_only_for_a_remote_resource_server() {
    let setup = Setup::new();
    let url = "http://127.0.0.1:1/mcp";
    let remote = setup.remote_spec("remote", url);
    crate::cache::write(
        &setup.cache(),
        "remote",
        &crate::cache::url_key(url),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: true,
        },
    );
    let plain_remote = setup.remote_spec("plain", "http://127.0.0.1:2/mcp");
    crate::cache::write(
        &setup.cache(),
        "plain",
        &crate::cache::url_key("http://127.0.0.1:2/mcp"),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: false,
        },
    );
    let started = setup.start(vec![remote, plain_remote, lazy_spec(&setup, "stdio", true)]);
    assert!(started.failed.is_empty());
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    let rows: &[(&str, Value, Vec<Effect>)] = &[
        (
            "a remote resource server",
            json!({"action": "list", "server": "remote"}),
            vec![Effect::Reads, Effect::Network],
        ),
        (
            "a stdio resource server",
            json!({"action": "list", "server": "stdio"}),
            vec![Effect::Reads],
        ),
        (
            "an unknown server",
            json!({"action": "list", "server": "missing"}),
            vec![Effect::Reads],
        ),
        ("no server", json!({"action": "list"}), vec![Effect::Reads]),
        (
            "a remote server without resources",
            json!({"action": "list", "server": "plain"}),
            vec![Effect::Reads],
        ),
    ];
    for (name, arguments, effects) in rows {
        let outcome = tool
            .effects(&args(arguments.clone()))
            .expect("classifiable");
        assert_eq!(&outcome.declared.effects, effects, "row: {name}");
        assert!(outcome.declared.reversible, "row: {name}");
        assert_eq!(outcome.declared.paths, None, "row: {name}");
        assert_eq!(outcome.subject, Some(String::new()), "row: {name}");
        assert_eq!(outcome.prefix, None, "row: {name}");
        assert!(!outcome.always_reviewed, "row: {name}");
    }
    setup.stop(started.servers);
}

/// One lazy spec whose command does not exist, cached with `resources`.
fn dead_spec(setup: &Setup, name: &str, resources: bool) -> ServerSpec {
    let mut spec = setup.spec(name);
    spec.stdio_mut().command = "/no/such/command".to_owned();
    spec.stdio_mut().args = Vec::new();
    crate::cache::write(
        &setup.cache(),
        name,
        &spec.key(),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources,
        },
    );
    spec
}

fn run(tool: &Arc<super::Resources>, arguments: Value) -> contract::tool::Output {
    let tool = Arc::clone(tool);
    let tool_arguments = args(arguments);
    fakes::within(
        "the mcp_resources call",
        Deadline::after(WITHIN),
        move || tool.run(&tool_arguments, &cancel(), &emit()),
    )
}

#[test]
fn an_invalid_request_fails_invalid_arguments_and_starts_nothing() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let started = setup.start(vec![
        dead_spec(&setup, "broken", true),
        dead_spec(&setup, "plain", false),
        lazy_spec(&setup, "fx", true),
    ]);
    assert!(started.failed.is_empty());
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    let out = run(&tool, json!({"action": "write"}));
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(failure.message, "`action` must be `list` or `read`.");
    assert!(out.servers.is_empty());
    assert!(!setup.spawned(), "an invalid request starts nothing",);
    setup.stop(started.servers);
}

#[test]
fn an_unknown_server_fails_invalid_arguments() {
    let setup = Setup::new();
    let started = setup.start(vec![
        dead_spec(&setup, "broken", true),
        dead_spec(&setup, "plain", false),
    ]);
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    for arguments in [
        json!({"action": "list", "server": "missing"}),
        json!({"action": "read", "server": "missing", "uri": "file:///x"}),
    ] {
        let out = run(&tool, arguments);
        let failure = out.error.expect("failed");
        assert_eq!(failure.code, ErrorCode::InvalidArguments);
        assert_eq!(
            failure.message,
            "No MCP server named `missing` offers resources; \
             `list` with no `server` names those that do.",
        );
        assert!(out.servers.is_empty());
    }
    setup.stop(started.servers);
}

#[test]
fn a_server_without_resources_fails_invalid_arguments() {
    let setup = Setup::new();
    let started = setup.start(vec![
        dead_spec(&setup, "broken", true),
        dead_spec(&setup, "plain", false),
    ]);
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    for arguments in [
        json!({"action": "list", "server": "plain"}),
        json!({"action": "read", "server": "plain", "uri": "file:///x"}),
    ] {
        let out = run(&tool, arguments);
        let failure = out.error.expect("failed");
        assert_eq!(failure.code, ErrorCode::InvalidArguments);
        assert_eq!(
            failure.message,
            "No MCP server named `plain` offers resources; \
             `list` with no `server` names those that do.",
        );
        assert!(out.servers.is_empty());
    }
    setup.stop(started.servers);
}

#[test]
fn a_server_that_fails_to_start_carries_its_record() {
    let setup = Setup::new();
    let started = setup.start(vec![
        dead_spec(&setup, "broken", true),
        dead_spec(&setup, "plain", false),
    ]);
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let tool = resources(&started, setup.artifacts().as_path(), &images);
    let out = run(&tool, json!({"action": "list", "server": "broken"}));
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpServerUnavailable);
    assert!(
        failure
            .message
            .starts_with("The MCP server `broken` failed to start: "),
        "message: {}",
        failure.message,
    );
    assert_eq!(out.servers.len(), 1);
    match &out.servers[0] {
        contract::tool::ServerRecord::Failed(record) => {
            assert_eq!(record.server, "broken");
            assert_eq!(record.error, failure);
        }
        contract::tool::ServerRecord::Ready(_) => panic!("one failed record"),
    }
    setup.stop(started.servers);
}

/// A fake remote MCP server offering resources: the handshake with the
/// tools and resources capabilities, one `fetch` tool answering a
/// resource link, two `resources/list` pages keyed on `params.cursor`,
/// and `resources/read` keyed on `params.uri`.
fn resource_server() -> fakes::ProviderServer {
    resource_server_dying_once(None)
}

/// [`resource_server`], except that the first request of `dying` drops its
/// connection: that call dies, and the next call restarts the server, so
/// its record is `Ready`.
fn resource_server_dying_once(dying: Option<&'static str>) -> fakes::ProviderServer {
    let died = std::sync::atomic::AtomicBool::new(false);
    fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result = |result: Value| {
            json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
        };
        let error = |code: i64, message: &str| {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                .to_string()
        };
        let method = body.get("method").and_then(Value::as_str);
        if method.is_some()
            && method == dying
            && !died.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return fakes::Response::drop_connection();
        }
        match method {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => fakes::Response::status(
                200,
                result(json!({"tools": [{"name": "fetch", "description": "Fetches.", "inputSchema": {"type": "object"}}]})),
            ),
            Some("tools/call") => fakes::Response::status(
                200,
                result(json!({"content": [{"type": "resource_link", "uri": "file:///notes.txt", "name": "notes", "mimeType": "text/plain"}]})),
            ),
            Some("resources/list") => {
                let page = match body
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(Value::as_str)
                {
                    None => json!({
                        "resources": [{"uri": "file:///notes.txt", "name": "notes", "mimeType": "text/plain"}],
                        "nextCursor": "p2",
                    }),
                    Some("p2") => json!({
                        "resources": [{"uri": "file:///more.txt", "name": "more", "mimeType": "text/plain"}],
                    }),
                    Some(cursor) => panic!("unexpected cursor {cursor}"),
                };
                fakes::Response::status(200, result(page))
            }
            Some("resources/read") => {
                let uri = body
                    .get("params")
                    .and_then(|params| params.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match uri {
                    "file:///notes.txt" => fakes::Response::status(
                        200,
                        result(json!({"contents": [{"uri": "file:///notes.txt", "mimeType": "text/plain", "text": "hello"}]})),
                    ),
                    "file:///blob.bin" => fakes::Response::status(
                        200,
                        result(json!({"contents": [{"uri": "file:///blob.bin", "mimeType": "application/octet-stream", "blob": "aGk="}]})),
                    ),
                    _ => fakes::Response::status(200, error(-32002, "Resource not found")),
                }
            }
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

/// Starts `name` over `server` through the public `start`, expecting the
/// tool declared.
fn live(setup: &Setup, server: &fakes::ProviderServer, name: &str) -> crate::start::Started {
    let url = format!("{}/mcp", server.url());
    let spec = setup.remote_spec(name, &url);
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = setup.cache();
    let clock = setup.clock();
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    fakes::within("the start", Deadline::after(WITHIN), move || {
        crate::start::start(
            vec![spec],
            &workspace,
            &artifacts,
            images,
            &cache,
            &clock,
            "0.0.0",
        )
    })
}

/// The `mcp_resources` tool of `started`.
fn live_tool(
    started: &crate::start::Started,
    artifacts: &std::path::Path,
) -> Arc<super::Resources> {
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    resources(started, artifacts, &images)
}

fn text_of(out: &contract::tool::Output) -> Vec<String> {
    out.content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Image { .. } => "<image>".to_owned(),
            ContentPart::Pdf(_) | ContentPart::Unknown => "<other>".to_owned(),
        })
        .collect()
}

/// The `params` of every request to `method` the fake recorded, in order.
fn method_params(server: &fakes::ProviderServer, method: &str) -> Vec<Value> {
    server
        .requests()
        .iter()
        .filter_map(|req| serde_json::from_slice::<Value>(&req.body).ok())
        .filter(|body| body.get("method").and_then(Value::as_str) == Some(method))
        .map(|body| body.get("params").cloned().unwrap_or(Value::Null))
        .collect()
}

#[test]
fn a_live_remote_server_with_resources_declares_the_tool() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    assert!(started.failed.is_empty());
    let names: Vec<String> = started
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect();
    assert_eq!(names, ["mcp__docs__fetch", "mcp_resources"]);
    setup.stop(started.servers);
}

#[test]
fn list_without_a_server_names_it_and_sends_nothing() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let out = run(&tool, json!({"action": "list"}));
    assert!(out.error.is_none());
    assert_eq!(text_of(&out), ["{\"servers\":[\"docs\"]}"]);
    assert!(
        method_params(&server, "resources/list").is_empty(),
        "naming the servers sends nothing",
    );
    setup.stop(started.servers);
}

#[test]
fn list_returns_the_page_and_the_cursor_fetches_the_next() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let first = run(&tool, json!({"action": "list", "server": "docs"}));
    assert!(first.error.is_none());
    assert_eq!(
        text_of(&first),
        [
            "{\"resources\":[{\"uri\":\"file:///notes.txt\",\"name\":\"notes\",\
          \"mimeType\":\"text/plain\"}],\"nextCursor\":\"p2\"}"
        ],
    );
    let second = run(
        &tool,
        json!({"action": "list", "server": "docs", "cursor": "p2"}),
    );
    assert!(second.error.is_none());
    assert_eq!(
        text_of(&second),
        [
            "{\"resources\":[{\"uri\":\"file:///more.txt\",\"name\":\"more\",\
          \"mimeType\":\"text/plain\"}]}"
        ],
    );
    assert_eq!(
        method_params(&server, "resources/list"),
        [json!({}), json!({"cursor": "p2"})],
    );
    setup.stop(started.servers);
}

#[test]
fn read_maps_text_and_blob_contents() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let text = run(
        &tool,
        json!({"action": "read", "server": "docs", "uri": "file:///notes.txt"}),
    );
    assert!(text.error.is_none());
    assert_eq!(text_of(&text), ["hello"]);
    let blob = run(
        &tool,
        json!({"action": "read", "server": "docs", "uri": "file:///blob.bin"}),
    );
    assert!(blob.error.is_none());
    let lines = text_of(&blob);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("Saved application/octet-stream content, 2 bytes, to artifacts/mcp-"),
        "line: {}",
        lines[0],
    );
    assert_eq!(
        method_params(&server, "resources/read"),
        [
            json!({"uri": "file:///notes.txt"}),
            json!({"uri": "file:///blob.bin"})
        ],
    );
    setup.stop(started.servers);
}

/// The one `Ready` record a call after a restart carries, for `docs`.
fn ready_docs() -> Vec<contract::tool::ServerRecord> {
    vec![contract::tool::ServerRecord::Ready(
        contract::events::McpServerReady {
            server: "docs".to_owned(),
        },
    )]
}

#[test]
fn list_after_a_restart_names_the_ready_server() {
    let setup = Setup::new();
    let server = resource_server_dying_once(Some("resources/list"));
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let died = run(&tool, json!({"action": "list", "server": "docs"}));
    assert!(died.error.is_some(), "the first list dies");
    let listed = run(&tool, json!({"action": "list", "server": "docs"}));
    assert!(listed.error.is_none());
    assert_eq!(listed.servers, ready_docs());
    setup.stop(started.servers);
}

#[test]
fn read_after_a_restart_names_the_ready_server() {
    let setup = Setup::new();
    let server = resource_server_dying_once(Some("resources/read"));
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let read = json!({"action": "read", "server": "docs", "uri": "file:///notes.txt"});
    let died = run(&tool, read.clone());
    assert!(died.error.is_some(), "the first read dies");
    let read = run(&tool, read);
    assert!(read.error.is_none());
    assert_eq!(text_of(&read), ["hello"]);
    assert_eq!(read.servers, ready_docs());
    setup.stop(started.servers);
}

#[test]
fn a_resource_link_a_tool_returned_reads_through_mcp_resources() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    let fetch = setup.tool(&started, "mcp__docs__fetch");
    let linked = setup.run(&fetch);
    assert!(linked.error.is_none());
    assert_eq!(
        text_of(&linked),
        ["Resource link: file:///notes.txt (name: notes, type: text/plain)"],
    );
    // No `list` page holds any other URI, yet `read` fetches the link.
    let tool = live_tool(&started, setup.artifacts().as_path());
    let out = run(
        &tool,
        json!({"action": "read", "server": "docs", "uri": "file:///notes.txt"}),
    );
    assert!(out.error.is_none());
    assert_eq!(text_of(&out), ["hello"]);
    setup.stop(started.servers);
}

#[test]
fn a_refused_read_is_a_tool_error_with_its_message() {
    let setup = Setup::new();
    let server = resource_server();
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let out = run(
        &tool,
        json!({"action": "read", "server": "docs", "uri": "file:///missing.txt"}),
    );
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert!(
        failure.message.contains("Resource not found"),
        "message: {}",
        failure.message,
    );
    setup.stop(started.servers);
}

#[test]
fn a_malformed_list_or_read_is_a_tool_error() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start_responding(|req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => fakes::Response::status(200, result(json!({"tools": []}))),
            Some("resources/list") => {
                fakes::Response::status(200, result(json!({"nextCursor": "p2"})))
            }
            Some("resources/read") => {
                fakes::Response::status(200, result(json!({"contents": {"text": "hi"}})))
            }
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap();
    let started = live(&setup, &server, "docs");
    let tool = live_tool(&started, setup.artifacts().as_path());
    let listed = run(&tool, json!({"action": "list", "server": "docs"}));
    let failure = listed.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert_eq!(
        failure.message,
        "The MCP server `docs` answered `resources/list` without a resource list.",
    );
    let read = run(
        &tool,
        json!({"action": "read", "server": "docs", "uri": "file:///x"}),
    );
    let failure = read.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert_eq!(
        failure.message,
        "The MCP server `docs` answered `resources/read` without its contents.",
    );
    setup.stop(started.servers);
}

/// A fake remote MCP server answering `resources/list` without a resource
/// list and `resources/read` without its contents, except that the first
/// request of `dying` drops its connection: that call dies, and the next
/// call restarts the server, so its record is `Ready`.
fn malformed_server_dying_once(dying: Option<&'static str>) -> fakes::ProviderServer {
    let died = std::sync::atomic::AtomicBool::new(false);
    fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        let method = body.get("method").and_then(Value::as_str);
        if method.is_some()
            && method == dying
            && !died.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return fakes::Response::drop_connection();
        }
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => fakes::Response::status(200, result(json!({"tools": []}))),
            Some("resources/list") => {
                fakes::Response::status(200, result(json!({"nextCursor": "p2"})))
            }
            Some("resources/read") => {
                fakes::Response::status(200, result(json!({"contents": {"text": "hi"}})))
            }
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

#[test]
fn a_malformed_answer_after_a_restart_carries_the_ready_server() {
    // A malformed answer still names the ready server, so the records are
    // nonempty: an already-running server would leave them empty. Each arm
    // runs against its own server, because only the call that restarts a
    // server carries its `Ready` record, and a second death would leave
    // the slot dead instead of restarting it again.
    assert!(!ready_docs().is_empty());
    for (method, arguments, message) in [
        (
            "resources/list",
            json!({"action": "list", "server": "docs"}),
            "The MCP server `docs` answered `resources/list` without a resource list.",
        ),
        (
            "resources/read",
            json!({"action": "read", "server": "docs", "uri": "file:///x"}),
            "The MCP server `docs` answered `resources/read` without its contents.",
        ),
    ] {
        let setup = Setup::new();
        let server = malformed_server_dying_once(Some(method));
        let started = live(&setup, &server, "docs");
        let tool = live_tool(&started, setup.artifacts().as_path());
        let died = run(&tool, arguments.clone());
        assert!(died.error.is_some(), "the first {method} dies");
        let out = run(&tool, arguments);
        let failure = out.error.expect("failed");
        assert_eq!(failure.code, ErrorCode::ToolError);
        assert_eq!(failure.message, message);
        assert_eq!(
            out.servers,
            ready_docs(),
            "malformed {method} keeps its records"
        );
        setup.stop(started.servers);
    }
}

#[test]
fn the_first_starts_tool_reaches_an_adopted_server() {
    let setup = Setup::new();
    let server = resource_server();
    let first = live(&setup, &server, "docs");
    assert!(first.failed.is_empty());
    let other = dead_spec(&setup, "other", true);
    let second = setup.start(vec![other]);
    assert!(second.failed.is_empty());
    first.servers.adopt(second.servers);
    let tool = live_tool(&first, setup.artifacts().as_path());
    // Both servers are named, although `other` started elsewhere.
    let named = run(&tool, json!({"action": "list"}));
    assert!(named.error.is_none());
    assert_eq!(text_of(&named), ["{\"servers\":[\"docs\",\"other\"]}"]);
    // `list` on `docs` reaches the fake.
    let listed = run(&tool, json!({"action": "list", "server": "docs"}));
    assert!(listed.error.is_none());
    assert!(
        text_of(&listed)[0].contains("file:///notes.txt"),
        "the adopted tool reaches its own server: {:?}",
        text_of(&listed),
    );
    // `other` is a lazy cached server whose command does not exist: its
    // call fails at once, proving the tool found the adopted slot and
    // served it instead of failing `invalid_arguments`.
    let refused = run(&tool, json!({"action": "list", "server": "other"}));
    let failure = refused.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpServerUnavailable);
    assert_eq!(refused.servers.len(), 1);
    match &refused.servers[0] {
        contract::tool::ServerRecord::Failed(record) => assert_eq!(record.server, "other"),
        contract::tool::ServerRecord::Ready(_) => panic!("one failed record"),
    }
    setup.stop(first.servers);
}

#[test]
fn read_asking_answers_an_elicitation_and_reads() {
    use contract::events::Answer;
    use contract::tool::Answered;
    // `run_asking` forwards its asker through the slot seam, as a tool's
    // does: the call is held until its ask is observed, so without
    // forwarding no ask ever arrives and this wait fails instead of the
    // remover's decline passing. The fake holds the read's answer until
    // the elicitation's answer POST arrives, so the ask is always
    // observed first: the answer below is the asker's, never the
    // remover's.
    let setup = Setup::new();
    let elicitation = json!({
        "jsonrpc": "2.0",
        "id": "el-1",
        "method": "elicitation/create",
        "params": {
            "message": "m",
            "requestedSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
        },
    })
    .to_string();
    let gated = fakes::GatedHttp::start_read(
        elicitation,
        json!({"contents": [{
            "uri": "file:///notes.txt",
            "mimeType": "text/plain",
            "text": "hello",
        }]}),
    );
    let spec = setup.remote_spec("docs", &gated.url());
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = setup.cache();
    let clock = setup.clock();
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let started = fakes::within("the start", Deadline::after(WITHIN), move || {
        crate::start::start(
            vec![spec],
            &workspace,
            &artifacts,
            images,
            &cache,
            &clock,
            "0.0.0",
        )
    });
    let tool = live_tool(&started, setup.artifacts().as_path());
    let (ask, seen, send) = crate::elicit::tests::FakeAsk::held("a_4");
    let moved = Arc::clone(&tool);
    let arguments = args(json!({"action": "read", "server": "docs", "uri": "file:///notes.txt"}));
    let (done, out) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(moved.run_asking(&arguments, &cancel(), &emit(), &ask))
            .expect("collected");
    });
    fakes::Deadline::after(WITHIN).recv_or_fail(&seen, "the elicitation's ask");
    send.send(Answered::Reply(Answer::Text {
        text: "prod".to_owned(),
    }))
    .expect("the answer sends");
    let out = fakes::Deadline::after(WITHIN)
        .recv(&out)
        .unwrap_or_else(|_| panic!("the asking read answers within {WITHIN:?}"));
    assert!(out.error.is_none(), "the read succeeds: {out:?}");
    assert_eq!(text_of(&out), ["hello"]);
    // The pump POSTs the elicitation's answer before the fake releases
    // the read's answer, so by the time the read completes the POST is
    // recorded: no wait is needed.
    let mut answers = gated.elicitation_answers().into_iter();
    let answer = answers.next().expect("one answer to the elicitation");
    assert!(answers.next().is_none(), "exactly one answer");
    // The ask is recorded before its answer is POSTed, and the call was
    // held until the ask arrived, so the answer is the asker's `accept`
    // with the content: no wait is needed and no decline passes.
    assert_eq!(
        answer.get("result"),
        Some(&json!({"action": "accept", "content": {"name": "prod"}})),
        "asked means accept with the content",
    );
    setup.stop(started.servers);
}
