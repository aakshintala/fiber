//! One tool's declaration and every run outcome, through the public
//! [`Tool`] trait.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Map, Value, json};

use super::McpTool;
use crate::effects::Hints;
use crate::server::Server;

/// How long a test waits for a thread or a child, in real time.
///
/// The largest round value that keeps every test's serial deadlines within
/// half of nextest's 120 s kill: the worst mcp test,
/// `server::tests::cancel_ends_the_wait_and_sends_cancelled`, can exhaust five
/// (5 x 10 s = 50 s <= 60 s). A passing run never waits on it; it only
/// bounds a hang.
const WITHIN: Duration = Duration::from_secs(10);

fn arguments() -> Map<String, Value> {
    Map::new()
}

fn declare(server: &str, tool: &str, hints: Hints, link: std::sync::Weak<Server>) -> McpTool {
    McpTool::declare(
        server,
        tool,
        "Echoes.".to_owned(),
        json!({"type": "object"}),
        &hints,
        Duration::from_secs(30),
        link,
    )
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
        vec![Effect::Reads, Effect::Network]
    );
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
}

#[test]
fn a_dead_link_is_unavailable() {
    let tool = declare("fx", "echo", Hints::default(), std::sync::Weak::new());
    let output = tool.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
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

fn start_within(
    script: &str,
    args: &[String],
    workspace: &std::path::Path,
    clock: &Arc<dyn Clock>,
) -> Server {
    // Threaded with a wall-clock limit: without `send`, `insert`,
    // `deliver` or `read_stdout` the handshake would sit parked on the
    // fake clock forever, so a bare direct start would hang the test
    // instead of failing it.
    let script = script.to_owned();
    let args = args.to_owned();
    let workspace = workspace.to_path_buf();
    let clock = Arc::clone(clock);
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = Server::start(
            &script,
            &args,
            &BTreeMap::new(),
            &workspace,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the server starts within {WITHIN:?}"))
        .expect("the fixture server starts")
        .server
}

struct Live {
    _dir: TempDir,
    fake: Arc<FakeClock>,
    server: Arc<Server>,
}

impl Live {
    fn tools(tools: &Value, files: &[(&str, &str)]) -> Self {
        let dir = TempDir::new("fiber-mcp-tool");
        std::fs::write(dir.path().join("tools.json"), tools.to_string()).expect("tools");
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).expect("result");
        }
        let fake = FakeClock::new();
        let clock: Arc<dyn Clock> = fake.clone();
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = dir.path().to_path_buf();
        let server = start_within(
            &script,
            &[workspace.display().to_string()],
            &workspace,
            &clock,
        );
        Self {
            _dir: dir,
            fake,
            server: Arc::new(server),
        }
    }

    fn tool(&self, tool: &str, hints: Hints, timeout: Duration) -> McpTool {
        McpTool::declare(
            "fx",
            tool,
            String::new(),
            json!({"type": "object"}),
            &hints,
            timeout,
            Arc::downgrade(&self.server),
        )
    }
}

#[test]
fn text_blocks_come_back_in_order() {
    let live = Live::tools(
        &json!([{"name": "echo"}]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#,
        )],
    );
    let tool = live.tool("echo", Hints::default(), Duration::from_secs(30));
    let output = tool.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![
            contract::shapes::ContentPart::Text {
                text: "a".to_owned()
            },
            contract::shapes::ContentPart::Text {
                text: "b".to_owned()
            },
        ],
    );
}

#[test]
fn non_text_parts_are_dropped() {
    let live = Live::tools(
        &json!([{"name": "show"}]),
        &[(
            "call-show.json",
            r#"{"content":[{"type":"text","text":"seen"},{"type":"image","data":"x"}]}"#,
        )],
    );
    let tool = live.tool("show", Hints::default(), Duration::from_secs(30));
    let output = tool.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![contract::shapes::ContentPart::Text {
            text: "seen".to_owned()
        }],
    );
}

#[test]
fn an_error_result_is_tool_error_with_the_text() {
    let live = Live::tools(
        &json!([{"name": "bad"}]),
        &[(
            "call-bad.json",
            r#"{"content":[{"type":"text","text":"no such thing"}],"isError":true}"#,
        )],
    );
    let tool = live.tool("bad", Hints::default(), Duration::from_secs(30));
    let output = tool.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::ToolError);
    assert_eq!(error.message, "no such thing");
    assert_eq!(
        output.content,
        vec![contract::shapes::ContentPart::Text {
            text: "no such thing".to_owned()
        }],
    );
}

#[test]
fn a_json_rpc_error_is_tool_error() {
    let live = Live::tools(&json!([{"name": "echo"}]), &[]);
    // No `call-missing.json`: the fixture answers `-32602`.
    let tool = live.tool("missing", Hints::default(), Duration::from_secs(30));
    let output = tool.run(
        &arguments(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::ToolError);
    assert!(
        error.message.contains("Unknown tool"),
        "message: {}",
        error.message
    );
}

#[test]
fn a_timed_out_call_is_timeout() {
    let live = Live::tools(&json!([{"name": "hang"}]), &[("call-hang.json", "hang")]);
    let tool = live.tool("hang", Hints::default(), Duration::from_secs(60));
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let output = tool.run(
                &arguments(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
        let deadline = live
            .fake
            .now()
            .checked_add(Duration::from_secs(60))
            .expect("deadline");
        assert!(
            live.fake.await_parked(deadline, WITHIN),
            "the call waits on its timeout",
        );
        live.fake.advance(Duration::from_secs(60));
        let output = result.recv_timeout(WITHIN).expect("the call ends");
        let error = output.error.expect("failed");
        assert_eq!(error.code, ErrorCode::Timeout);
        assert_eq!(
            error.message,
            "The MCP server `fx` did not answer `hang` within 60000 ms."
        );
    });
}

#[test]
fn a_cancelled_call_is_mcp_cancel_requested() {
    let live = Live::tools(&json!([{"name": "hang"}]), &[("call-hang.json", "hang")]);
    let tool = live.tool("hang", Hints::default(), Duration::from_secs(60));
    let cancel = fakes::CancelToken::new();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let output = tool.run(&arguments(), &cancel, &fakes::Recorder::default());
            done.send(output).expect("collected");
        });
        let deadline = live
            .fake
            .now()
            .checked_add(Duration::from_secs(60))
            .expect("deadline");
        assert!(
            live.fake.await_parked(deadline, WITHIN),
            "the call waits on its timeout",
        );
        cancel.cancel();
        let output = result.recv_timeout(WITHIN).expect("the call ends");
        let error = output.error.expect("failed");
        assert_eq!(error.code, ErrorCode::McpCancelRequested);
        assert_eq!(
            error.message,
            "The call to `hang` on the MCP server `fx` was cancelled; the server may still act on it."
        );
    });
}
