//! One tool's declaration and every run outcome, with no child process.

use std::time::Duration;

use contract::ErrorCode;
use contract::events::{McpServerFailed, McpServerReady, ServerFailure};
use contract::shapes::ContentPart;
use contract::tool::{ServerRecord, Tool};
use serde_json::{Map, Value, json};

use super::{Call, answer, output};
use crate::effects::Hints;
use crate::error::McpError;
use crate::server_json::{CallResult, Content, Resource};
use crate::slot::{Death, Fault};
use crate::test_support::WITHIN;
use fakes::Deadline;

fn arguments() -> Map<String, Value> {
    Map::new()
}

fn declare(
    server: &str,
    tool: &str,
    hints: Hints,
    slot: std::sync::Weak<crate::slot::Slot>,
    artifacts: &std::path::Path,
) -> super::McpTool {
    super::McpTool::declare(
        server,
        tool,
        "Echoes.".to_owned(),
        json!({"type": "object"}),
        &hints,
        false,
        super::Target {
            timeout: Duration::from_secs(30),
            artifacts: artifacts.to_path_buf(),
            images: crate::test_support::FakeImages::ok(),
            slot,
        },
    )
}

fn call(artifacts: &std::path::Path) -> Call {
    call_with_images(artifacts, crate::test_support::FakeImages::ok())
}

fn call_with_images(
    artifacts: &std::path::Path,
    images: std::sync::Arc<crate::test_support::FakeImages>,
) -> Call {
    Call {
        server: "fx".to_owned(),
        tool: "echo".to_owned(),
        timeout: Duration::from_secs(30),
        artifacts: artifacts.to_path_buf(),
        images,
        slot: std::sync::Weak::new(),
    }
}

#[test]
fn declared_tools_carry_the_artifacts_dir() {
    let dir = fakes::TempDir::new("mcp-tool-artifacts");
    let tool = declare(
        "fx",
        "echo",
        Hints::default(),
        std::sync::Weak::new(),
        dir.path(),
    );
    assert_eq!(tool.call.artifacts, dir.path());
}

#[test]
fn the_definition_carries_the_qualified_name_schema_and_no_deferral() {
    let dir = fakes::TempDir::new("mcp-tool-definition");
    let tool = declare(
        "fx",
        "echo",
        Hints::default(),
        std::sync::Weak::new(),
        dir.path(),
    );
    let definition = tool.definition();
    assert_eq!(definition.name, "mcp__fx__echo");
    assert_eq!(definition.description, "Echoes.");
    assert_eq!(definition.input_schema, json!({"type": "object"}));
    assert!(!definition.deferred);
}

#[test]
fn effects_come_from_the_resolved_hints_for_any_arguments() {
    let dir = fakes::TempDir::new("mcp-tool-effects");
    let tool = declare(
        "fx",
        "echo",
        Hints {
            read_only: Some(true),
            destructive: None,
            open_world: None,
        },
        std::sync::Weak::new(),
        dir.path(),
    );
    let effects = tool.effects(&arguments()).expect("classifiable");
    assert_eq!(
        effects.declared.effects,
        vec![
            contract::shapes::Effect::Reads,
            contract::shapes::Effect::Network
        ]
    );
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
}

#[test]
fn a_dead_link_is_unavailable() {
    let dir = fakes::TempDir::new("mcp-tool-dead-link");
    let tool = declare(
        "fx",
        "echo",
        Hints::default(),
        std::sync::Weak::new(),
        dir.path(),
    );
    let output = fakes::within(
        "the call to `echo` on a dead server",
        Deadline::after(WITHIN),
        move || {
            tool.run(
                &arguments(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            )
        },
    );
    assert_eq!(
        output.error.as_ref().map(|error| &error.code),
        Some(&ErrorCode::McpServerUnavailable)
    );
    assert_eq!(
        output.error.as_ref().map(|error| error.message.as_str()),
        Some("The MCP server `fx` did not start, or it has since exited.")
    );
}

fn text_part(text: Option<&str>) -> Content {
    Content::Text {
        text: text.map(str::to_owned),
    }
}

fn link_part(uri: Option<&str>, name: Option<&str>, mime_type: Option<&str>) -> Content {
    Content::ResourceLink {
        uri: uri.map(str::to_owned),
        name: name.map(str::to_owned),
        mime_type: mime_type.map(str::to_owned),
    }
}

fn resource_part(
    text: Option<&str>,
    blob: Option<Value>,
    uri: Option<&str>,
    mime_type: Option<&str>,
) -> Content {
    Content::Resource {
        resource: Resource {
            text: text.map(str::to_owned),
            blob,
            uri: uri.map(str::to_owned),
            mime_type: mime_type.map(str::to_owned),
        },
    }
}

fn audio_part(data: Option<Value>, mime_type: Option<&str>) -> Content {
    Content::Audio {
        data,
        mime_type: mime_type.map(str::to_owned),
    }
}

fn image_part(data: Option<Value>, mime_type: Option<&str>) -> Content {
    Content::Image {
        data,
        mime_type: mime_type.map(str::to_owned),
    }
}

fn cancel() -> fakes::CancelToken {
    fakes::CancelToken::new()
}

fn text_line(text: &str) -> ContentPart {
    ContentPart::Text {
        text: text.to_owned(),
    }
}

/// The `Saved` line `answer` writes for `bytes` of `mime`: the same
/// content-addressed name, so the table pins the exact line.
fn saved_line(mime: &str, bytes: &[u8]) -> ContentPart {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    text_line(&format!(
        "Saved {mime} content, {} bytes, to artifacts/mcp-{}.bin",
        bytes.len(),
        &hex[..16],
    ))
}

#[test]
#[allow(clippy::type_complexity, reason = "one table pins every answer shape")]
fn answer_maps_each_part_to_its_own_text_in_order() {
    let dir = fakes::TempDir::new("mcp-tool-answer");
    let call = call(dir.path());
    let rows: &[(
        &str,
        CallResult,
        Vec<ContentPart>,
        Option<(ErrorCode, &str)>,
    )] = &[
        (
            "two texts",
            CallResult {
                content: vec![text_part(Some("a")), text_part(Some("b"))],
                is_error: false,
            },
            vec![text_line("a"), text_line("b")],
            None,
        ),
        (
            "a missing text is dropped",
            CallResult {
                content: vec![text_part(Some("seen")), text_part(None)],
                is_error: false,
            },
            vec![text_line("seen")],
            None,
        ),
        (
            "an empty text is kept",
            CallResult {
                content: vec![text_part(Some(""))],
                is_error: false,
            },
            vec![text_line("")],
            None,
        ),
        (
            "other and unreadable become unsupported",
            CallResult {
                content: vec![Content::Other, Content::Unreadable],
                is_error: false,
            },
            vec![
                text_line("Unsupported content."),
                text_line("Unsupported content."),
            ],
            None,
        ),
        (
            "a link names its uri, name and type",
            CallResult {
                content: vec![link_part(Some("file:///x"), Some("x"), Some("text/plain"))],
                is_error: false,
            },
            vec![text_line(
                "Resource link: file:///x (name: x, type: text/plain)",
            )],
            None,
        ),
        (
            "a link omits an absent name and type",
            CallResult {
                content: vec![link_part(Some("file:///x"), None, None)],
                is_error: false,
            },
            vec![text_line("Resource link: file:///x")],
            None,
        ),
        (
            "a link omits an empty name",
            CallResult {
                content: vec![link_part(Some("file:///x"), Some(""), Some("text/plain"))],
                is_error: false,
            },
            vec![text_line("Resource link: file:///x (type: text/plain)")],
            None,
        ),
        (
            "a link without a uri says so",
            CallResult {
                content: vec![link_part(None, Some("x"), Some("text/plain"))],
                is_error: false,
            },
            vec![text_line("Resource link: (no uri)")],
            None,
        ),
        (
            "a text resource becomes its text",
            CallResult {
                content: vec![resource_part(
                    Some("from a file"),
                    None,
                    Some("file:///x"),
                    Some("text/plain"),
                )],
                is_error: false,
            },
            vec![text_line("from a file")],
            None,
        ),
        (
            "text wins over a blob",
            CallResult {
                content: vec![resource_part(
                    Some("seen"),
                    Some(Value::from("aGk=")),
                    None,
                    None,
                )],
                is_error: false,
            },
            vec![text_line("seen")],
            None,
        ),
        (
            "a blob resource is saved",
            CallResult {
                content: vec![resource_part(None, Some(Value::from("aGk=")), None, None)],
                is_error: false,
            },
            vec![saved_line("application/octet-stream", b"hi")],
            None,
        ),
        (
            "audio data is saved under its type",
            CallResult {
                content: vec![audio_part(Some(Value::from("aGk=")), Some("audio/mpeg"))],
                is_error: false,
            },
            vec![saved_line("audio/mpeg", b"hi")],
            None,
        ),
        (
            "audio without data is reported",
            CallResult {
                content: vec![text_part(Some("seen")), audio_part(None, None)],
                is_error: false,
            },
            vec![
                text_line("seen"),
                text_line("Could not save application/octet-stream content: no data"),
            ],
            None,
        ),
        (
            "a non-string blob is reported",
            CallResult {
                content: vec![resource_part(None, Some(Value::from(7)), None, None)],
                is_error: false,
            },
            vec![text_line(
                "Could not save application/octet-stream content: expected a base64 string",
            )],
            None,
        ),
        (
            "a resource with neither text nor blob is unreadable",
            CallResult {
                content: vec![resource_part(None, None, None, None)],
                is_error: false,
            },
            vec![text_line("An unreadable resource.")],
            None,
        ),
        (
            "an error carries its text parts only",
            CallResult {
                content: vec![
                    text_part(Some("no such thing")),
                    link_part(Some("file:///x"), None, None),
                    resource_part(None, None, None, None),
                ],
                is_error: true,
            },
            vec![
                text_line("no such thing"),
                text_line("Resource link: file:///x"),
                text_line("An unreadable resource."),
            ],
            Some((ErrorCode::ToolError, "no such thing")),
        ),
        (
            "an empty error names the tool",
            CallResult {
                content: vec![link_part(Some("file:///x"), None, None)],
                is_error: true,
            },
            vec![text_line("Resource link: file:///x")],
            Some((
                ErrorCode::ToolError,
                "The MCP server `fx` reported an error for `echo`.",
            )),
        ),
    ];
    for (name, result, content, error) in rows {
        let out = answer(&call, result, &cancel());
        assert_eq!(&out.content, content, "row: {name}");
        match error {
            None => assert!(out.error.is_none(), "row: {name}"),
            Some((code, message)) => {
                let failure = out.error.expect("failed");
                assert_eq!(failure.code, *code, "row: {name}");
                assert_eq!(failure.message, *message, "row: {name}");
            }
        }
    }
}

/// The saved file's name, read back from its `Saved` line: the line
/// gives the path from the session directory, and the test directory
/// stands in for the artifacts dir itself.
fn saved_path(line: &str) -> &str {
    line.strip_prefix("Saved ")
        .and_then(|line| line.split_once(", ").map(|(_, rest)| rest))
        .and_then(|line| line.split_once(", to ").map(|(_, rest)| rest))
        .and_then(|path| path.strip_prefix("artifacts/"))
        .expect("a Saved line")
}

#[test]
fn binary_content_is_written_to_a_content_addressed_file() {
    use base64::Engine as _;
    let dir = fakes::TempDir::new("mcp-tool-artifacts-write");
    let call = call(dir.path());
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"hi");
    let result = CallResult {
        content: vec![resource_part(None, Some(Value::from(encoded)), None, None)],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    assert!(out.error.is_none());
    assert_eq!(out.content.len(), 1);
    let ContentPart::Text { text } = &out.content[0] else {
        panic!("one text line");
    };
    let path = dir.path().join(saved_path(text));
    let read = std::fs::read(&path).expect("the artifact is written");
    assert_eq!(read, b"hi");
    // A repeated result rewrites the same file.
    let again = answer(&call, &result, &cancel());
    assert_eq!(again.content, out.content);
    let read = std::fs::read(&path).expect("the artifact is rewritten");
    assert_eq!(read, b"hi");
}

#[test]
fn binary_failures_report_without_failing_the_call() {
    let dir = fakes::TempDir::new("mcp-tool-artifacts-unwritable");
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "in the way").expect("the blocker exists");
    let rows: &[(&str, Call, CallResult, &str)] = &[
        (
            "bad base64",
            call(dir.path()),
            CallResult {
                content: vec![resource_part(None, Some(Value::from("!!!")), None, None)],
                is_error: false,
            },
            "Could not save application/octet-stream content: ",
        ),
        (
            "a write failure",
            call(&blocker),
            CallResult {
                content: vec![audio_part(Some(Value::from("aGk=")), Some("audio/mpeg"))],
                is_error: false,
            },
            "Could not save audio/mpeg content: ",
        ),
    ];
    for (name, call, result, prefix) in rows {
        let out = answer(call, result, &cancel());
        assert!(out.error.is_none(), "row: {name}");
        assert_eq!(out.content.len(), 1, "row: {name}");
        let ContentPart::Text { text } = &out.content[0] else {
            panic!("one text line, row: {name}");
        };
        assert!(
            text.starts_with(prefix) && text.len() > prefix.len(),
            "row: {name}, line: {text}",
        );
    }
}

fn died_record() -> Death {
    let error = McpError::ServerExited {
        server: "fx".to_owned(),
    };
    Death {
        record: McpServerFailed {
            server: "fx".to_owned(),
            reason: ServerFailure::Died,
            will_restart: true,
            error: contract::shapes::Failure::from(&error),
        },
        error,
    }
}

#[test]
#[allow(clippy::type_complexity, reason = "one table pins every outcome")]
fn each_outcome_maps_to_its_code_sentence_and_servers() {
    let dir = fakes::TempDir::new("mcp-tool-outcome");
    let base = call(dir.path());
    let timeout_call = Call {
        timeout: Duration::from_secs(60),
        ..call(dir.path())
    };
    let ok: Value = json!({"content": [{"type": "text", "text": "hi"}]});
    let rows: Vec<(
        &str,
        &Call,
        Result<Value, Fault>,
        Vec<ServerRecord>,
        Option<ErrorCode>,
        Option<&str>,
        usize,
    )> = vec![
        ("ok", &base, Ok(ok), vec![], None, None, 0),
        (
            "timeout",
            &timeout_call,
            Err(Fault::Timeout),
            vec![],
            Some(ErrorCode::Timeout),
            Some("The MCP server `fx` did not answer `echo` within 60000 ms."),
            0,
        ),
        (
            "cancelled",
            &base,
            Err(Fault::Cancelled),
            vec![],
            Some(ErrorCode::McpCancelRequested),
            Some(
                "The call to `echo` on the MCP server `fx` was cancelled; the server may still act on it.",
            ),
            0,
        ),
        (
            "refused",
            &base,
            Err(Fault::Refused("Unknown tool: echo.".to_owned())),
            vec![],
            Some(ErrorCode::ToolError),
            Some("Unknown tool: echo."),
            0,
        ),
        (
            "died",
            &base,
            Err(Fault::Died(died_record())),
            vec![],
            Some(ErrorCode::McpServerUnavailable),
            Some("The MCP server `fx` exited; Fiber restarts it on the next call."),
            1,
        ),
        (
            "gone",
            &base,
            Err(Fault::Gone),
            vec![],
            Some(ErrorCode::McpServerUnavailable),
            Some("The MCP server `fx` did not start, or it has since exited."),
            0,
        ),
        (
            "http_refused",
            &base,
            Err(Fault::Refused("The server answered HTTP 500.".to_owned())),
            vec![],
            Some(ErrorCode::ToolError),
            Some("The server answered HTTP 500."),
            0,
        ),
        (
            "expired",
            &base,
            Err(Fault::Expired),
            vec![],
            Some(ErrorCode::McpServerUnavailable),
            Some("The MCP server `fx` ended its session; Fiber starts a new one on the next call."),
            0,
        ),
    ];
    for (name, call, called, servers, code, message, records) in rows {
        // Every row runs with one incoming ready record before its own,
        // so the mapping must carry incoming records through.
        let ready = ServerRecord::Ready(McpServerReady {
            server: "fx".to_owned(),
        });
        let mut incoming = vec![ready.clone()];
        incoming.extend(servers);
        let mut expected = vec![ready];
        if name == "died" {
            expected.push(ServerRecord::Failed(died_record().record));
        }
        let out = output(call, called, incoming, &cancel());

        match (code, message) {
            (None, None) => {
                assert!(out.error.is_none(), "row: {name}");
                assert_eq!(
                    out.content,
                    vec![ContentPart::Text {
                        text: "hi".to_owned()
                    }],
                    "row: {name}"
                );
                assert_eq!(out.servers, expected, "row: {name}");
            }
            (Some(code), Some(message)) => {
                let failure = out.error.expect("failed");
                assert_eq!(failure.code, code, "row: {name}");
                assert_eq!(failure.message, message, "row: {name}");
                assert_eq!(out.servers.len(), records + 1, "row: {name}");
                assert_eq!(out.servers, expected, "row: {name}");
                if name == "died" {
                    match &out.servers[1] {
                        ServerRecord::Failed(record) => {
                            assert_eq!(record.server, "fx");
                            assert_eq!(record.reason, ServerFailure::Died);
                            assert_eq!(record.error, failure);
                        }
                        ServerRecord::Ready(_) => panic!("one death record, got ready"),
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}

fn image_bytes() -> Vec<u8> {
    b"raw-image".to_vec()
}

fn image_encoded() -> Value {
    use base64::Engine as _;
    Value::from(base64::engine::general_purpose::STANDARD.encode(image_bytes()))
}

fn image_expected() -> Vec<ContentPart> {
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
}

#[test]
fn an_image_part_is_processed_once_and_keeps_its_bytes() {
    let dir = fakes::TempDir::new("mcp-tool-image");
    let images = crate::test_support::FakeImages::with(Ok(contract::provider::ImageRef {
        path: "artifacts/mcp-fake.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width: 2,
        height: 3,
    }));
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![image_part(Some(image_encoded()), Some("image/png"))],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    assert!(out.error.is_none());
    assert_eq!(out.content, image_expected());
    assert_eq!(images.calls(), vec![image_bytes()]);
}

#[test]
fn image_parts_keep_server_order() {
    let dir = fakes::TempDir::new("mcp-tool-image-order");
    let images = crate::test_support::FakeImages::ok();
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![
            text_part(Some("before")),
            image_part(Some(image_encoded()), Some("image/png")),
            text_part(Some("after")),
        ],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        vec![
            ContentPart::Text {
                text: "before".to_owned()
            },
            ContentPart::Text {
                text: "Image: 2x3 image/png.\n".to_owned(),
            },
            ContentPart::Image {
                path: "artifacts/mcp-fake.png".to_owned(),
                mime_type: "image/png".to_owned(),
                width: 2,
                height: 3,
            },
            ContentPart::Text {
                text: "after".to_owned()
            },
        ]
    );
    assert_eq!(images.call_count(), 1);
}

#[test]
fn an_unreadable_image_fails_without_processing_later_parts() {
    let dir = fakes::TempDir::new("mcp-tool-image-unreadable");
    let images = crate::test_support::FakeImages::with(Err(
        contract::images::ImageError::Unreadable("nope: not a png".to_owned()),
    ));
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![
            image_part(Some(image_encoded()), Some("image/png")),
            image_part(Some(image_encoded()), Some("image/png")),
        ],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::UnsupportedFile);
    assert!(
        failure.message.contains("nope: not a png"),
        "message: {}",
        failure.message,
    );
    assert!(
        failure.message.contains("image"),
        "message: {}",
        failure.message,
    );
    assert_eq!(images.call_count(), 1);
}

#[test]
fn a_failed_image_ends_tool_error_with_its_message() {
    let dir = fakes::TempDir::new("mcp-tool-image-failed");
    let images = crate::test_support::FakeImages::with(Err(contract::images::ImageError::Failed(
        "the child blew up".to_owned(),
    )));
    let call = call_with_images(dir.path(), images);
    let result = CallResult {
        content: vec![image_part(Some(image_encoded()), Some("image/png"))],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::ToolError);
    assert_eq!(failure.message, "the child blew up");
}

#[test]
fn a_cancelled_image_ends_like_a_cancelled_call() {
    let dir = fakes::TempDir::new("mcp-tool-image-cancelled");
    let images =
        crate::test_support::FakeImages::with(Err(contract::images::ImageError::Cancelled));
    let call = call_with_images(dir.path(), images);
    let result = CallResult {
        content: vec![image_part(Some(image_encoded()), Some("image/png"))],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::McpCancelRequested);
    assert_eq!(
        failure.message,
        "The call to `echo` on the MCP server `fx` was cancelled; the server may still act on it.",
    );
}

#[test]
fn bad_image_data_fails_unsupported() {
    let dir = fakes::TempDir::new("mcp-tool-image-bad-data");
    for (name, data, message) in [
        ("missing data", None, "image content: no data"),
        (
            "non-string data",
            Some(Value::from(7)),
            "image content: expected a base64 string",
        ),
    ] {
        let images = crate::test_support::FakeImages::ok();
        let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
        let result = CallResult {
            content: vec![image_part(data, Some("image/png"))],
            is_error: false,
        };
        let out = answer(&call, &result, &cancel());
        let failure = out.error.expect("failed");
        assert_eq!(failure.code, ErrorCode::UnsupportedFile, "row: {name}");
        assert_eq!(failure.message, message, "row: {name}");
        assert_eq!(images.call_count(), 0, "row: {name}");
    }
    let images = crate::test_support::FakeImages::ok();
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![image_part(Some(Value::from("!!!")), Some("image/png"))],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    let failure = out.error.expect("failed");
    assert_eq!(failure.code, ErrorCode::UnsupportedFile);
    assert!(
        failure.message.starts_with("image content: ")
            && failure.message.len() > "image content: ".len(),
        "message: {}",
        failure.message,
    );
    assert_eq!(images.call_count(), 0);
}

#[test]
fn an_embedded_image_resource_is_processed_once() {
    let dir = fakes::TempDir::new("mcp-tool-image-resource");
    let images = crate::test_support::FakeImages::ok();
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![resource_part(
            None,
            Some(image_encoded()),
            None,
            Some("image/png"),
        )],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    assert!(out.error.is_none());
    assert_eq!(out.content, image_expected());
    assert_eq!(images.calls(), vec![image_bytes()]);
}

#[test]
fn only_image_typed_blobs_take_the_image_path() {
    use base64::Engine as _;
    let encoded = Value::from(base64::engine::general_purpose::STANDARD.encode(b"hi"));
    for (name, mime, processes) in [
        ("image/png", Some("image/png"), true),
        ("bare image slash", Some("image/"), true),
        ("application image", Some("application/image/x"), false),
        ("pdf", Some("application/pdf"), false),
        ("octet stream", Some("application/octet-stream"), false),
        ("missing mime", None, false),
    ] {
        let dir = fakes::TempDir::new("mcp-tool-image-mime");
        let images = crate::test_support::FakeImages::ok();
        let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
        let result = CallResult {
            content: vec![resource_part(None, Some(encoded.clone()), None, mime)],
            is_error: false,
        };
        let out = answer(&call, &result, &cancel());
        assert!(out.error.is_none(), "row: {name}");
        if processes {
            assert_eq!(out.content, image_expected(), "row: {name}");
            assert_eq!(images.call_count(), 1, "row: {name}");
        } else {
            assert_eq!(out.content.len(), 1, "row: {name}");
            let ContentPart::Text { text } = &out.content[0] else {
                panic!("one text line, row: {name}");
            };
            assert!(
                text.starts_with("Saved ") && text.contains("artifacts/mcp-"),
                "row: {name}, line: {text}",
            );
            assert_eq!(images.call_count(), 0, "row: {name}");
        }
    }
}

#[test]
fn resource_text_wins_over_an_image_blob() {
    let dir = fakes::TempDir::new("mcp-tool-image-text-wins");
    let images = crate::test_support::FakeImages::ok();
    let call = call_with_images(dir.path(), std::sync::Arc::clone(&images));
    let result = CallResult {
        content: vec![resource_part(
            Some("seen"),
            Some(image_encoded()),
            None,
            Some("image/png"),
        )],
        is_error: false,
    };
    let out = answer(&call, &result, &cancel());
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        vec![ContentPart::Text {
            text: "seen".to_owned()
        }]
    );
    assert_eq!(images.call_count(), 0);
}

/// One eliciting fixture server and its `ask` tool: `elicit-ask.json`
/// asks `name` then `approve`.
fn eliciting_setup() -> (crate::test_support::Setup, crate::start::Started) {
    use crate::test_support::Setup;
    let setup = Setup::with_tools(&json!([{"name": "ask"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.write(
        "elicit-ask.json",
        r#"{"message":"Deploy settings","requestedSchema":{"type":"object","properties":{"name":{"type":"string","title":"Name"},"approve":{"type":"boolean"}},"required":["name"]}}"#,
    );
    let spec = setup.spec("fx");
    let started = setup.start(vec![spec]);
    (setup, started)
}

#[test]
fn run_without_an_asker_registers_nothing_and_the_reader_declines() {
    // `run` passes no asker, so the call is not registered: the reader
    // declines the elicitation at once, and the call returns its result.
    // Without the registration gate the reader would strand it.
    let (setup, started) = eliciting_setup();
    let tool = setup.tool(&started, "mcp__fx__ask");
    let output = setup.run(&tool);
    assert!(output.error.is_none(), "the call returns its result");
    // The fixture answers the call only on reading the `decline`, so the
    // result proves it is logged: no barrier.
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let decline = log
        .lines()
        .find(|line| line.contains("\"el-") && line.contains("\"action\":\"decline\""))
        .unwrap_or_else(|| panic!("the reader declines: {log}"));
    assert!(
        decline.contains(r#""result":{"action":"decline"}"#),
        "{decline}"
    );
    setup.stop(started.servers);
}

#[test]
fn run_asking_asks_naming_the_configured_server() {
    use contract::events::{Answer, Interaction};
    use contract::tool::Answered;
    // `run_asking` passes the asker with the configured server name, so
    // each prompt names it.
    let (setup, started) = eliciting_setup();
    let tool = setup.tool(&started, "mcp__fx__ask");
    let (ask, seen) = crate::elicit::tests::FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Confirmed { confirmed: true }),
        ],
    );
    let moved = tool.clone();
    let output = fakes::within("the asking call", Deadline::after(WITHIN), move || {
        moved.run_asking(
            &Default::default(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
            &ask,
        )
    });
    assert!(output.error.is_none(), "the call returns its result");
    let wait = fakes::Deadline::after(WITHIN);
    let first = wait.recv_or_fail(&seen, "the first field's ask");
    match first.interaction {
        Interaction::TextInput { prompt } => assert!(
            prompt.starts_with("The MCP server `fx` asks: "),
            "the prompt names the server: {prompt}",
        ),
        Interaction::Confirm { .. }
        | Interaction::Select { .. }
        | Interaction::MultiSelect { .. }
        | Interaction::Form { .. } => panic!("the string field asks a text input"),
    }
    wait.recv_or_fail(&seen, "the second field's ask");
    setup.stop(started.servers);
}

#[test]
fn an_open_world_tool_declares_unknown_hosts() {
    let dir = fakes::TempDir::new("mcp-hosts");
    let tool = declare(
        "fx",
        "echo",
        Hints {
            read_only: Some(true),
            destructive: None,
            open_world: Some(true),
        },
        std::sync::Weak::new(),
        dir.path(),
    );
    let effects = tool.effects(&arguments()).unwrap();
    assert_eq!(effects.hosts, None);
    assert!(
        effects
            .declared
            .effects
            .contains(&contract::shapes::Effect::Network)
    );
}
