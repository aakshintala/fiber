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
) -> super::McpTool {
    super::McpTool::declare(
        server,
        tool,
        "Echoes.".to_owned(),
        json!({"type": "object"}),
        &hints,
        Duration::from_secs(30),
        slot,
    )
}

fn call() -> Call {
    Call {
        server: "fx".to_owned(),
        tool: "echo".to_owned(),
        timeout: Duration::from_secs(30),
        slot: std::sync::Weak::new(),
    }
}

#[test]
fn the_definition_carries_the_qualified_name_schema_and_no_deferral() {
    let tool = declare("fx", "echo", Hints::default(), std::sync::Weak::new());
    let definition = tool.definition();
    assert_eq!(definition.name, "mcp__fx__echo");
    assert_eq!(definition.description, "Echoes.");
    assert_eq!(definition.input_schema, json!({"type": "object"}));
    assert!(!definition.deferred);
}

#[test]
fn effects_come_from_the_resolved_hints_for_any_arguments() {
    let tool = declare(
        "fx",
        "echo",
        Hints {
            read_only: Some(true),
            destructive: None,
            open_world: None,
        },
        std::sync::Weak::new(),
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
    let tool = declare("fx", "echo", Hints::default(), std::sync::Weak::new());
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

#[test]
#[allow(clippy::type_complexity, reason = "one table pins every answer shape")]
fn answer_reads_text_in_order_and_drops_the_rest() {
    let rows: &[(&str, CallResult, Vec<ContentPart>, Option<(ErrorCode, &str)>)] = &[
        (
            "two texts",
            CallResult {
                content: vec![text_part(Some("a")), text_part(Some("b"))],
                is_error: false,
            },
            vec![
                ContentPart::Text {
                    text: "a".to_owned(),
                },
                ContentPart::Text {
                    text: "b".to_owned(),
                },
            ],
            None,
        ),
        (
            "a missing text is dropped",
            CallResult {
                content: vec![text_part(Some("seen")), text_part(None)],
                is_error: false,
            },
            vec![ContentPart::Text {
                text: "seen".to_owned(),
            }],
            None,
        ),
        (
            "an empty text is kept",
            CallResult {
                content: vec![text_part(Some(""))],
                is_error: false,
            },
            vec![ContentPart::Text {
                text: String::new(),
            }],
            None,
        ),
        (
            "non-text is dropped",
            CallResult {
                content: vec![text_part(Some("seen")), Content::Image],
                is_error: false,
            },
            vec![ContentPart::Text {
                text: "seen".to_owned(),
            }],
            None,
        ),
        (
            "an error carries its text",
            CallResult {
                content: vec![text_part(Some("no such thing"))],
                is_error: true,
            },
            vec![ContentPart::Text {
                text: "no such thing".to_owned(),
            }],
            Some((ErrorCode::ToolError, "no such thing")),
        ),
        (
            "an empty error names the tool",
            CallResult {
                content: vec![],
                is_error: true,
            },
            vec![],
            Some((
                ErrorCode::ToolError,
                "The MCP server `fx` reported an error for `echo`.",
            )),
        ),
        (
            "a resource is dropped",
            CallResult {
                content: vec![
                    text_part(Some("seen")),
                    Content::Resource {
                        resource: Resource {
                            text: Some("file".to_owned()),
                            blob: None,
                        },
                    },
                ],
                is_error: false,
            },
            vec![ContentPart::Text {
                text: "seen".to_owned(),
            }],
            None,
        ),
    ];
    for (name, result, content, error) in rows {
        let out = answer("fx", "echo", result);
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
    let base = call();
    let timeout_call = Call {
        timeout: Duration::from_secs(60),
        ..call()
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
        (
            "ok",
            &base,
            Ok(ok),
            vec![],
            None,
            None,
            0,
        ),
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
            Some("The call to `echo` on the MCP server `fx` was cancelled; the server may still act on it."),
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
