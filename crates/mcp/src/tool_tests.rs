//! One tool's declaration and every run outcome, with no child process.

use std::time::Duration;

use contract::ErrorCode;
use contract::events::{McpServerFailed, ServerFailure};
use contract::shapes::ContentPart;
use contract::tool::{ServerRecord, Tool};
use serde_json::{Map, Value, json};

use super::{Call, answer, output};
use crate::effects::Hints;
use crate::server_json::{CallResult, Content, Resource};
use crate::slot::Fault;
use crate::test_support::WITHIN;

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
        super::Target {
            timeout: Duration::from_secs(30),
            artifacts: artifacts.to_path_buf(),
            slot,
        },
    )
}

fn call(artifacts: &std::path::Path) -> Call {
    Call {
        server: "fx".to_owned(),
        tool: "echo".to_owned(),
        timeout: Duration::from_secs(30),
        artifacts: artifacts.to_path_buf(),
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
    let output = fakes::within("the call to `echo` on a dead server", WITHIN, move || {
        tool.run(
            &arguments(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        )
    });
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
            "an image is dropped",
            CallResult {
                content: vec![text_part(Some("seen")), Content::Image],
                is_error: false,
            },
            vec![text_line("seen")],
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
            "audio without data is dropped",
            CallResult {
                content: vec![text_part(Some("seen")), audio_part(None, None)],
                is_error: false,
            },
            vec![text_line("seen")],
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
        let out = answer(&call, result);
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
    let out = answer(&call, &result);
    assert!(out.error.is_none());
    assert_eq!(out.content.len(), 1);
    let ContentPart::Text { text } = &out.content[0] else {
        panic!("one text line");
    };
    let path = dir.path().join(saved_path(text));
    let read = std::fs::read(&path).expect("the artifact is written");
    assert_eq!(read, b"hi");
    // A repeated result rewrites the same file.
    let again = answer(&call, &result);
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
        let out = answer(call, result);
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

fn died_record() -> McpServerFailed {
    McpServerFailed {
        server: "fx".to_owned(),
        reason: ServerFailure::Died,
        will_restart: true,
        error: crate::fail::failure(
            ErrorCode::McpServerUnavailable,
            "The MCP server `fx` exited; Fiber restarts it on the next call.".to_owned(),
        ),
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
    ];
    for (name, call, called, servers, code, message, records) in rows {
        let out = output(call, called, servers);
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
            }
            (Some(code), Some(message)) => {
                let failure = out.error.expect("failed");
                assert_eq!(failure.code, code, "row: {name}");
                assert_eq!(failure.message, message, "row: {name}");
                assert_eq!(out.servers.len(), records, "row: {name}");
                if name == "died" {
                    match &out.servers[0] {
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
