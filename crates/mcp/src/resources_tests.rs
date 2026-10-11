//! The `mcp_resources` tool: argument parsing, page and content mapping,
//! effects and refusals, with no child process unless a test starts one.

use std::sync::Arc;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use serde_json::{Map, Value, json};

use super::{Request, contents, page, request, servers_text};
use crate::start::{DEFAULT_CALL_TIMEOUT, ServerSpec};
use crate::test_support::{FakeImages, Setup, WITHIN};

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
    let rows: &[(&str, Value, Result<Request, &str>)] = &[
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
            Err("`read` needs `server` and `uri`."),
        ),
        (
            "read without a server fails",
            json!({"action": "read", "uri": "u"}),
            Err("`read` needs `server` and `uri`."),
        ),
        (
            "another action fails",
            json!({"action": "write"}),
            Err("`action` must be `list` or `read`."),
        ),
        (
            "no action fails",
            json!({}),
            Err("`action` must be `list` or `read`."),
        ),
        (
            "a non-string action fails",
            json!({"action": 7}),
            Err("`action` must be `list` or `read`."),
        ),
        (
            "a non-string server fails",
            json!({"action": "list", "server": 5}),
            Err("`server` must be a string."),
        ),
        (
            "a non-string uri fails",
            json!({"action": "read", "server": "docs", "uri": 5}),
            Err("`uri` must be a string."),
        ),
        (
            "a non-string cursor fails",
            json!({"action": "list", "server": "docs", "cursor": 5}),
            Err("`cursor` must be a string."),
        ),
    ];
    for (name, arguments, expected) in rows {
        let parsed = request(&args(arguments.clone()));
        match expected {
            Ok(expected) => assert_eq!(&parsed.expect("parsed"), expected, "row: {name}"),
            Err(message) => assert_eq!(&parsed.expect_err("rejected"), message, "row: {name}"),
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
            Err(
                "The MCP server `docs` answered `resources/list` without a resource list."
                    .to_owned()
            ),
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
    fakes::within("the mcp_resources call", WITHIN, move || {
        tool.run(&tool_arguments, &cancel(), &emit())
    })
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
