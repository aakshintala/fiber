//! The server's prompt list through the fixture and a fake clock: every
//! wait carries a named deadline, and the clock advances only after the
//! log proves the handshake is paging on it.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::TempDir;
use serde_json::{Value, json};

use crate::server::{Server, StartError};
use crate::test_support::{Setup, WITHIN};

impl Setup {
    fn cursor_forever(&self, method: &str) {
        self.write("cursor-forever", method);
    }
}


fn write(dir: &TempDir, name: &str, content: &str) {
    std::fs::write(dir.path().join(name), content).expect("fixture file");
}

fn greet_prompts() -> Value {
    json!([{
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    }])
}

#[test]
fn a_server_with_prompts_lists_them_at_start() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&greet_prompts());
    let open = setup.start_expect(Duration::from_secs(5));
    assert_eq!(
        open.listed.prompts,
        serde_json::from_value::<Vec<crate::server_json::ListedPrompt>>(greet_prompts())
            .expect("prompts read")
    );
    assert_eq!(open.listed.tools.len(), 1);
    assert_eq!(open.listed.tools[0].name, "echo");
    let log = setup.requests();
    let initialized = log
        .find("notifications/initialized")
        .expect("the handshake notifies initialized");
    let listed = log
        .find(r#""method":"prompts/list""#)
        .expect("the handshake lists prompts");
    assert!(
        initialized < listed,
        "prompts/list runs after notifications/initialized",
    );
    open.server.stop();
}

#[test]
fn a_server_without_the_prompts_capability_is_never_asked_for_prompts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let open = setup.start_expect(Duration::from_secs(5));
    assert!(open.listed.prompts.is_empty());
    assert!(
        !setup.requests().contains(r#""method":"prompts/list""#),
        "no prompts/list without the capability",
    );
    open.server.stop();
}

#[test]
fn tool_listing_follows_the_tools_capability() {
    // Present: the tools list as usual, and the handshake asks for them.
    let present = Setup::new();
    present.tools(&json!([{"name": "echo"}]));
    let open = present.start_expect(Duration::from_secs(5));
    assert_eq!(open.listed.tools.len(), 1);
    assert!(
        present.requests().contains(r#""method":"tools/list""#),
        "the handshake lists tools when advertised",
    );
    open.server.stop();
    // Absent (`tools.json` holding `error`): the start succeeds with no
    // tools, and `tools/list` is never sent, so its error answer never
    // matters.
    let absent = Setup::new();
    write(&absent.dir, "tools.json", "error");
    let open = absent.start_expect(Duration::from_secs(5));
    assert!(open.listed.tools.is_empty());
    assert!(
        !absent.requests().contains(r#""method":"tools/list""#),
        "no tools/list without the capability",
    );
    open.server.stop();
}

#[test]
fn a_prompt_only_server_lists_prompts_with_no_tools() {
    // A server advertising only prompts: no tools capability, and
    // `tools/list` would answer -32601. The start succeeds, prompts
    // list, and `tools/list` is never sent.
    let setup = Setup::new();
    setup.write("tools.json", "error");
    setup.prompts(&greet_prompts());
    let open = setup.start_expect(Duration::from_secs(5));
    assert!(open.listed.tools.is_empty());
    assert_eq!(
        open.listed.prompts,
        serde_json::from_value::<Vec<crate::server_json::ListedPrompt>>(greet_prompts())
            .expect("prompts read")
    );
    let log = setup.requests();
    assert!(
        log.contains(r#""method":"prompts/list""#),
        "the handshake lists prompts: {log}"
    );
    assert!(
        !log.contains(r#""method":"tools/list""#),
        "the handshake never lists tools: {log}"
    );
    open.server.stop();
}

#[test]
fn a_failing_prompt_list_leaves_the_server_started_with_no_prompts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts_raw("error");
    let open = setup.start_expect(Duration::from_secs(5));
    assert!(open.listed.prompts.is_empty());
    assert_eq!(open.listed.tools.len(), 1, "the tools still list");
    open.server.stop();
}

#[test]
fn endless_pages_end_at_the_startup_deadline() {
    // Tools page before prompts in one handshake, so only the prompt
    // list pages forever: the tools pages end on their own.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&greet_prompts());
    setup.cursor_forever("prompts/list");
    let timeout = Duration::from_secs(5);
    let (done, result) = mpsc::channel();
    let script = fakes::mcp_fixture().display().to_string();
    let workspace = setup.dir.path().to_path_buf();
    let arg = workspace.display().to_string();
    let clock = setup.clock();
    thread::spawn(move || {
        let outcome = Server::start(
            &script,
            &[arg],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    setup.await_requests(r#""method":"prompts/list""#, 1);
    setup.await_requests(r#""method":"prompts/list""#, 2);
    let pid = setup.pid();
    setup.fake.advance(timeout + Duration::from_millis(1));
    let outcome = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the paging start ends within {WITHIN:?}"));
    assert!(
        matches!(outcome, Err(StartError::Deadline)),
        "endless prompt pages end at the startup deadline",
    );
    setup.await_reaped(pid);
}

#[test]
fn endless_tool_pages_end_at_the_startup_deadline() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.cursor_forever("tools/list");
    let timeout = Duration::from_secs(5);
    let (done, result) = mpsc::channel();
    let script = fakes::mcp_fixture().display().to_string();
    let workspace = setup.dir.path().to_path_buf();
    let arg = workspace.display().to_string();
    let clock = setup.clock();
    thread::spawn(move || {
        let outcome = Server::start(
            &script,
            &[arg],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    setup.await_requests(r#""method":"tools/list""#, 2);
    let pid = setup.pid();
    setup.fake.advance(timeout + Duration::from_millis(1));
    let outcome = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the paging start ends within {WITHIN:?}"));
    assert!(
        matches!(outcome, Err(StartError::Deadline)),
        "endless tool pages end at the startup deadline",
    );
    setup.await_reaped(pid);
}

#[test]
fn runnable_names_hold_no_whitespace() {
    use crate::server_json::ListedPrompt;
    let runnable = |name: &str| {
        serde_json::from_value::<ListedPrompt>(json!({"name": name}))
            .expect("a prompt reads")
            .runnable()
    };
    assert!(!runnable(""));
    assert!(!runnable("a b"));
    assert!(!runnable("a\tb"));
    assert!(runnable("greet"));
    assert!(runnable("a/b"));
}

#[test]
fn the_hint_marks_required_arguments() {
    use crate::server_json::Argument;
    use super::hint;
    assert_eq!(hint(&[]), None);
    assert_eq!(
        hint(&[
            Argument {
                name: "who".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "tone".to_owned(),
                required: false,
                rest: Default::default(),
            },
        ]),
        Some("<who> [tone]".to_owned())
    );
}

#[test]
fn fill_gives_one_word_per_argument_with_the_rest_to_the_last() {
    use crate::server_json::Argument;
    use super::fill;
    let args = || {
        vec![
            Argument {
                name: "who".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "tone".to_owned(),
                required: false,
                rest: Default::default(),
            },
        ]
    };
    let filled = fill(&args(), "Ada warm and kind").expect("filled");
    assert_eq!(
        filled.named,
        serde_json::Map::from_iter([
            ("who".to_owned(), json!("Ada")),
            ("tone".to_owned(), json!("warm and kind")),
        ])
    );
    assert_eq!(filled.appended, None);
    // The last argument takes the rest trimmed at both ends; an empty
    // last is absent.
    let filled = fill(&args(), "Ada  ").expect("filled");
    assert_eq!(
        filled.named,
        serde_json::Map::from_iter([("who".to_owned(), json!("Ada"))])
    );
    // Missing required names list in order.
    let missing = fill(&args(), "  \t ").expect_err("missing");
    assert_eq!(missing, ["who"]);
    let missing = fill(
        &[
            Argument {
                name: "a".to_owned(),
                required: true,
                rest: Default::default(),
            },
            Argument {
                name: "b".to_owned(),
                required: true,
                rest: Default::default(),
            },
        ],
        "",
    )
    .expect_err("missing");
    assert_eq!(missing, ["a", "b"]);
}

#[test]
fn fill_with_no_arguments_appends_the_text() {
    use super::fill;
    let filled = fill(&[], "extra words").expect("filled");
    assert!(filled.named.is_empty());
    assert_eq!(filled.appended, Some("extra words".to_owned()));
    let filled = fill(&[], "   ").expect("filled");
    assert_eq!(filled.appended, None);
    let filled = fill(&[], "").expect("filled");
    assert_eq!(filled.appended, None);
}

fn read_text(result: Value) -> Result<String, String> {
    use super::text;
    use crate::server_json::PromptResult;
    match serde_json::from_value::<PromptResult>(result) {
        Err(_) => Err("no messages".to_owned()),
        Ok(result) => text(&result),
    }
}

#[test]
fn text_joins_every_message_in_order() {
    assert_eq!(
        read_text(json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": "First."}},
            {"role": "assistant", "content": {"type": "text", "text": "Second."}},
        ]})),
        Ok("First.\n\nSecond.".to_owned())
    );
    assert_eq!(
        read_text(json!({"messages": [
            {"role": "user", "content": {"type": "resource", "resource": {"text": "From a file."}}},
        ]})),
        Ok("From a file.".to_owned())
    );
    for (result, kind) in [
        (
            json!({"messages": [{"role": "user", "content": {"type": "image", "data": "aGk="}}]}),
            "an image",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "audio", "data": "aGk="}}]}),
            "audio",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "resource", "resource": {"blob": "aGk="}}}]}),
            "a binary resource",
        ),
        (
            json!({"messages": [{"role": "user", "content": {"type": "resource_link", "uri": "file:///x"}}]}),
            "a resource link",
        ),
        (json!({}), "no messages"),
        (json!({"messages": []}), "no text"),
        (
            json!({"messages": [{"role": "user", "content": {"type": "text", "text": ""}}, {"role": "user"}]}),
            "no text",
        ),
    ] {
        assert_eq!(read_text(result), Err(kind.to_owned()), "kind: {kind}");
    }
}

#[test]
fn embedded_resource_text_and_blob_guard_are_distinct() {
    assert_eq!(
        read_text(
            json!({"messages": [{"content": {"type": "resource", "resource": {"text": "Available text"}}}]})
        ),
        Ok("Available text".to_owned()),
    );
    assert_eq!(
        read_text(
            json!({"messages": [{"content": {"type": "resource", "resource": {"blob": "aGk="}}}]})
        ),
        Err("a binary resource".to_owned()),
    );
    assert_eq!(
        read_text(json!({"messages": [{"content": {"type": "resource", "resource": {}}}]})),
        Err("an unreadable resource".to_owned()),
    );
}

impl Setup {
    fn get_line(&self) -> String {
        self.requests()
            .lines()
            .find(|line| line.contains(r#""method":"prompts/get""#))
            .expect("a prompts/get line")
            .to_owned()
    }
}


fn greet_entry() -> Value {
    json!({
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    })
}

fn greet_session() -> Setup {
    let session = Setup::new();
    session.tools(&json!([{"name": "echo"}]));
    session.prompts(&json!([greet_entry()]));
    session.prompt_result(
        "greet",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Say hello to Ada, warmly."}}]}"#,
    );
    session
}

#[test]
fn get_sends_the_named_arguments_and_returns_the_text() {
    use contract::shapes::ContentPart;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    assert!(started.failed.is_empty());
    let output = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        [ContentPart::Text {
            text: "Say hello to Ada, warmly.".to_owned(),
        }]
    );
    assert!(output.servers.is_empty());
    assert!(
        session
            .get_line()
            .contains(r#""arguments":{"tone":"warm","who":"Ada"}"#),
        "the get sends the named arguments: {}",
        session.get_line(),
    );
    session.stop(started.servers);
}

#[test]
fn get_on_a_lazy_server_starts_it() {
    let session = greet_session();
    let first = session.start(vec![session.spec("fx")]);
    assert!(first.failed.is_empty());
    session.stop(first.servers);
    std::fs::remove_file(session.dir.path().join("pid.txt")).expect("pid.txt");
    let second = session.start(vec![session.spec("fx")]);
    assert!(
        !session.dir.path().join("pid.txt").exists(),
        "declaring from the cache spawns nothing",
    );
    let output = session.get(
        &second.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    assert!(output.error.is_none());
    assert!(
        session.dir.path().join("pid.txt").exists(),
        "the first prompt run starts the server",
    );
    session.stop(second.servers);
}

#[test]
fn get_returns_the_failed_start_record_for_a_cached_prompt() {
    use contract::ErrorCode;
    use contract::events::{McpServerFailed, ServerFailure};
    use contract::shapes::Failure;
    use contract::tool::ServerRecord;

    let session = greet_session();
    let first = session.start(vec![session.spec("fx")]);
    assert!(first.failed.is_empty());
    session.stop(first.servers);

    // The cache keeps the listed prompt available without starting its
    // server, so `get` observes this failed lazy start.
    session.write("fail-start", "");
    let started = session.start(vec![session.spec("fx")]);
    assert!(started.failed.is_empty());
    let output = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );

    assert_eq!(
        output.error.expect("the prompt start fails").code,
        ErrorCode::McpPromptFailed
    );
    assert_eq!(
        output.servers,
        [ServerRecord::Failed(McpServerFailed {
            server: "fx".to_owned(),
            reason: ServerFailure::StartFailed,
            will_restart: true,
            error: Failure {
                code: ErrorCode::McpServerUnavailable,
                message: "The MCP server `fx` failed to start: The server's `initialize` reply was not a result. Check its `command` and `args` under `mcp.servers` in your configuration.".to_owned(),
                retry_after_ms: None,
                provider: None,
            },
        })],
        "the failed start record is returned in the observed order",
    );
    session.stop(started.servers);
}

#[test]
fn a_missing_required_argument_is_invalid_and_never_reaches_the_server() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    let output = session.get(
        &started.prompts,
        "fx",
        "greet",
        "",
        &fakes::CancelToken::new(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::InvalidArguments);
    assert_eq!(
        error.message,
        "The MCP server `fx`'s prompt `/greet` needs <who>. Run it as `/greet <who> [tone]`.",
    );
    assert!(
        !session.requests().contains("prompts/get"),
        "the server is never asked",
    );
    session.stop(started.servers);
}

#[test]
fn a_prompt_with_no_arguments_gets_the_text_after_its_own() {
    use contract::shapes::ContentPart;
    let session = Setup::new();
    session.tools(&json!([{"name": "echo"}]));
    session.prompts(&json!([{"name": "motd", "description": "The message."}]));
    session.prompt_result(
        "motd",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Body."}}]}"#,
    );
    let started = session.start(vec![session.spec("fx")]);
    let output = session.get(
        &started.prompts,
        "fx",
        "motd",
        "extra words",
        &fakes::CancelToken::new(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        [ContentPart::Text {
            text: "Body.\n\nextra words".to_owned(),
        }]
    );
    session.stop(started.servers);
}

#[test]
fn a_json_rpc_error_fails_the_prompt() {
    use contract::ErrorCode;
    let session = Setup::new();
    session.tools(&json!([{"name": "echo"}]));
    session.prompts(&json!([greet_entry(), {"name": "refused", "description": "Refused."}]));
    let started = session.start(vec![session.spec("fx")]);
    // No `prompt-refused.json`: the fixture refuses with -32602.
    let output = session.get(
        &started.prompts,
        "fx",
        "refused",
        "",
        &fakes::CancelToken::new(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert_eq!(
        error.message,
        "The MCP server `fx` refused the prompt `/refused`: Unknown prompt: refused.",
    );
    assert!(output.servers.is_empty());
    session.stop(started.servers);
}

#[test]
fn a_prompt_no_server_lists_fails_without_asking() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    let output = session.get(
        &started.prompts,
        "fx",
        "missing",
        "",
        &fakes::CancelToken::new(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert_eq!(
        error.message,
        "The MCP server `fx` has no prompt `/missing`.",
    );
    assert!(
        !session.requests().contains("prompts/get"),
        "the server is never asked",
    );
    session.stop(started.servers);
}

#[test]
fn a_server_that_dies_mid_get_is_recorded_once_and_restarts_on_the_next() {
    use contract::ErrorCode;
    use contract::events::{McpServerReady, ServerFailure};
    use contract::tool::ServerRecord;
    let session = greet_session();
    session.prompt_result("greet", "exit");
    let started = session.start(vec![session.spec("fx")]);
    let first = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    let error = first.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert!(
        error.message.contains("was not run: "),
        "the message names the server, the prompt and the cause: {}",
        error.message,
    );
    assert_eq!(first.servers.len(), 1, "the death is recorded once");
    let [ServerRecord::Failed(failed)] = &first.servers[..] else {
        panic!("one failure record: {:?}", first.servers);
    };
    assert_eq!(failed.server, "fx");
    assert_eq!(failed.reason, ServerFailure::Died);
    assert_eq!(failed.error.code, ErrorCode::McpServerUnavailable);
    assert!(failed.will_restart);
    // The file replaced, the next run restarts the server and reads it.
    session.prompt_result(
        "greet",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Back."}}]}"#,
    );
    let second = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    assert!(second.error.is_none());
    assert_eq!(
        second.servers,
        [ServerRecord::Ready(McpServerReady {
            server: "fx".to_owned(),
        })],
        "a successful get carries the restart record",
    );
    session.stop(started.servers);
}

#[test]
fn a_hung_get_times_out_only_after_the_clock_passes_the_call_timeout() {
    use contract::ErrorCode;
    let session = greet_session();
    session.prompt_result("greet", "hang");
    let timeout = crate::start::DEFAULT_CALL_TIMEOUT;
    let deadline = session.fake.now().checked_add(timeout).expect("deadline");
    let started = session.start(vec![session.spec("fx")]);
    let prompts = started.prompts.clone();
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let output = prompts.get("fx", "greet", "Ada warm", &cancel);
        done.send(output).expect("collected");
    });
    assert!(
        session.fake.await_parked(deadline, WITHIN),
        "the get waits on the call timeout within {WITHIN:?}",
    );
    let mark = session.fake.advance_marked(Duration::from_secs(599));
    assert!(
        session
            .fake
            .await_parked_since(&mark, Some(deadline), WITHIN),
        "the get waits again a second before its deadline within {WITHIN:?}",
    );
    assert!(
        result.try_recv().is_err(),
        "the get is still waiting a second before its deadline",
    );
    session.fake.advance(Duration::from_secs(1));
    let output = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the timed-out get answers within {WITHIN:?}"));
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert_eq!(
        error.message,
        "The MCP server `fx` did not answer the prompt `/greet` within 600000 ms.",
    );
    session.stop(started.servers);
}

#[test]
fn a_cancel_during_get_sends_cancelled_and_fails_the_prompt() {
    use contract::ErrorCode;
    let session = greet_session();
    session.prompt_result("greet", "hang");
    let timeout = crate::start::DEFAULT_CALL_TIMEOUT;
    let deadline = session.fake.now().checked_add(timeout).expect("deadline");
    let started = session.start(vec![session.spec("fx")]);
    let prompts = started.prompts.clone();
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    {
        let cancel = cancel.clone();
        thread::spawn(move || {
            let output = prompts.get("fx", "greet", "Ada warm", &cancel);
            done.send(output).expect("collected");
        });
    }
    assert!(
        session.fake.await_parked(deadline, WITHIN),
        "the get waits on the call timeout within {WITHIN:?}",
    );
    cancel.cancel();
    let output = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the cancelled get answers within {WITHIN:?}"));
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert!(
        error.message.contains("was cancelled"),
        "the message says the run was cancelled: {}",
        error.message,
    );
    // A later answer proves the cancel reached the server's log.
    session.prompt_result(
        "greet",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Back."}}]}"#,
    );
    let after = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    assert!(after.error.is_none());
    let log = session.requests();
    let cancelled = log
        .find("notifications/cancelled")
        .expect("the cancel is sent");
    let answered = log
        .rfind(r#""method":"prompts/get""#)
        .expect("the later get");
    assert!(
        cancelled < answered,
        "the cancel is logged before the later answer",
    );
    session.stop(started.servers);
}

#[test]
fn a_dead_slot_fails_the_prompt_without_spawning() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    session.stop(started.servers);
    std::fs::remove_file(session.dir.path().join("pid.txt")).expect("pid.txt");
    let output = session.get(
        &started.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert!(
        !session.dir.path().join("pid.txt").exists(),
        "a dead server never spawns again",
    );
}
