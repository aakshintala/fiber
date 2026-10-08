//! The server's prompt list through the fixture and a fake clock: every
//! wait carries a named deadline, and the clock advances only after the
//! log proves the handshake is paging on it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::server::{Server, StartError};

/// How long a test waits for a thread or a child, in real time.
const WITHIN: Duration = Duration::from_secs(10);

/// One real-time poll of a file or a child's exit.
const POLL: Duration = Duration::from_millis(50);

/// Poll iterations that span one `WITHIN` of `POLL` sleeps.
const POLLS: u128 = WITHIN.as_millis() / POLL.as_millis();

struct Setup {
    dir: TempDir,
    fake: Arc<FakeClock>,
}

impl Setup {
    fn new() -> Self {
        Self {
            dir: TempDir::new("fiber-mcp-prompts"),
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    fn tools(&self, tools: &Value) {
        write(&self.dir, "tools.json", &tools.to_string());
    }

    fn prompts(&self, prompts: &str) {
        write(&self.dir, "prompts.json", prompts);
    }

    fn cursor_forever(&self, method: &str) {
        write(&self.dir, "cursor-forever", method);
    }

    fn start_result(&self, timeout: Duration) -> Result<crate::server::OpenServer, StartError> {
        // Threaded with a wall-clock limit: an endlessly paging server
        // would sit looping on the fake clock forever, so a bare direct
        // start would hang the test instead of failing it.
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = self.dir.path().to_path_buf();
        let arg = workspace.display().to_string();
        let clock = self.clock();
        let (done, result) = mpsc::channel();
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
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
    }

    fn start(&self, timeout: Duration) -> crate::server::OpenServer {
        self.start_result(timeout)
            .expect("the fixture server starts")
    }

    fn requests(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("requests.log")).unwrap_or_default()
    }

    /// Polls `requests.log` until it holds `count` lines naming `method`,
    /// at most `WITHIN`: a passing run sees them, and a silent server fails
    /// the test instead of hanging it.
    fn await_requests(&self, method: &str, count: usize) {
        let (_held, probe) = mpsc::channel::<()>();
        for _ in 0..POLLS {
            if self.requests().matches(method).count() >= count {
                return;
            }
            match probe.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for {count} `{method}` lines");
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// Polls until `kill -0` fails for `pid`, at most `WITHIN`: the
    /// handshake's failure path reaps the child it started.
    fn await_reaped(&self, pid: u32) {
        let (_held, probe) = mpsc::channel::<()>();
        for _ in 0..POLLS {
            if !fakes::kill_pid(pid, "0").expect("probe") {
                return;
            }
            match probe.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for pid {pid} to be reaped");
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
    setup.prompts(&greet_prompts().to_string());
    let open = setup.start(Duration::from_secs(5));
    assert_eq!(
        open.prompts,
        greet_prompts().as_array().cloned().unwrap_or_default()
    );
    assert_eq!(
        open.tools,
        json!([{"name": "echo"}])
            .as_array()
            .cloned()
            .unwrap_or_default()
    );
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
    let open = setup.start(Duration::from_secs(5));
    assert!(open.prompts.is_empty());
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
    let open = present.start(Duration::from_secs(5));
    assert_eq!(open.tools.len(), 1);
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
    let open = absent.start(Duration::from_secs(5));
    assert!(open.tools.is_empty());
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
    write(&setup.dir, "tools.json", "error");
    setup.prompts(&greet_prompts().to_string());
    let open = setup.start(Duration::from_secs(5));
    assert!(open.tools.is_empty());
    assert_eq!(
        open.prompts,
        greet_prompts().as_array().cloned().unwrap_or_default()
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
    setup.prompts("error");
    let open = setup.start(Duration::from_secs(5));
    assert!(open.prompts.is_empty());
    assert_eq!(open.tools.len(), 1, "the tools still list");
    open.server.stop();
}

#[test]
fn endless_pages_end_at_the_startup_deadline() {
    // Tools page before prompts in one handshake, so only the prompt
    // list pages forever: the tools pages end on their own.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&greet_prompts().to_string());
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
fn a_prompt_entry_reads_with_defaults() {
    let read = super::ListedPrompt::read(&json!({"name": "greet"}));
    assert_eq!(read.name, "greet");
    assert_eq!(read.description, String::new());
    assert!(read.arguments.is_empty());
    let read = super::ListedPrompt::read(
        &json!({"name": 7, "description": 7, "arguments": [{"required": true}, {"name": "", "required": true}]}),
    );
    assert_eq!(read.name, String::new());
    assert_eq!(read.description, String::new());
    assert!(read.arguments.is_empty(), "a nameless argument is dropped");
}

#[test]
fn runnable_names_hold_no_whitespace() {
    use super::ListedPrompt;
    let runnable = |name: &str| ListedPrompt::read(&json!({"name": name})).runnable();
    assert!(!runnable(""));
    assert!(!runnable("a b"));
    assert!(!runnable("a\tb"));
    assert!(runnable("greet"));
    assert!(runnable("a/b"));
}

#[test]
fn the_hint_marks_required_arguments() {
    use super::{Argument, hint};
    assert_eq!(hint(&[]), None);
    assert_eq!(
        hint(&[
            Argument {
                name: "who".to_owned(),
                required: true
            },
            Argument {
                name: "tone".to_owned(),
                required: false
            },
        ]),
        Some("<who> [tone]".to_owned())
    );
}

#[test]
fn fill_gives_one_word_per_argument_with_the_rest_to_the_last() {
    use super::{Argument, fill};
    let args = || {
        vec![
            Argument {
                name: "who".to_owned(),
                required: true,
            },
            Argument {
                name: "tone".to_owned(),
                required: false,
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
            },
            Argument {
                name: "b".to_owned(),
                required: true,
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

#[test]
fn text_joins_every_message_in_order() {
    use super::text;
    assert_eq!(
        text(&json!({"messages": [
            {"role": "user", "content": {"type": "text", "text": "First."}},
            {"role": "assistant", "content": {"type": "text", "text": "Second."}},
        ]})),
        Ok("First.\n\nSecond.".to_owned())
    );
    assert_eq!(
        text(&json!({"messages": [
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
        assert_eq!(text(&result), Err(kind.to_owned()), "kind: {kind}");
    }
}

#[test]
fn embedded_resource_text_and_blob_guard_are_distinct() {
    use super::text;

    assert_eq!(
        text(
            &json!({"messages": [{"content": {"type": "resource", "resource": {"text": "Available text"}}}]})
        ),
        Ok("Available text".to_owned()),
    );
    assert_eq!(
        text(
            &json!({"messages": [{"content": {"type": "resource", "resource": {"blob": "aGk="}}}]})
        ),
        Err("a binary resource".to_owned()),
    );
    assert_eq!(
        text(&json!({"messages": [{"content": {"type": "resource", "resource": {}}}]})),
        Err("an unreadable resource".to_owned()),
    );
}

struct Session {
    dir: TempDir,
    fake: Arc<FakeClock>,
}

impl Session {
    fn new() -> Self {
        Self {
            dir: TempDir::new("fiber-mcp-get"),
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    fn tools(&self, tools: &Value) {
        write(&self.dir, "tools.json", &tools.to_string());
    }

    fn prompts(&self, prompts: &Value) {
        write(&self.dir, "prompts.json", &prompts.to_string());
    }

    fn prompt_result(&self, name: &str, body: &str) {
        write(&self.dir, &format!("prompt-{name}.json"), body);
    }

    fn spec(&self, name: &str) -> crate::start::ServerSpec {
        crate::start::ServerSpec {
            name: name.to_owned(),
            command: fakes::mcp_fixture().display().to_string(),
            args: vec![self.dir.path().display().to_string()],
            env: BTreeMap::new(),
            startup_timeout: crate::start::DEFAULT_STARTUP_TIMEOUT,
            call_timeout: crate::start::DEFAULT_CALL_TIMEOUT,
            enabled: None,
            disabled: Vec::new(),
            hints: BTreeMap::new(),
            required: false,
        }
    }

    fn start(&self, specs: Vec<crate::start::ServerSpec>) -> crate::start::Started {
        // Threaded with a wall-clock limit: a silent server would sit
        // parked on the fake clock forever, so a bare direct start would
        // hang the test instead of failing it.
        let workspace = self.dir.path().to_path_buf();
        let cache = workspace.join("cache");
        let clock = self.clock();
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let started = crate::start::start(specs, &workspace, &cache, &clock, "0.0.0");
            done.send(started).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
    }

    fn get(
        prompts: &crate::prompt::Prompts,
        server: &str,
        prompt: &str,
        arguments: &str,
        cancel: &fakes::CancelToken,
    ) -> contract::tool::Output {
        // Threaded with a wall-clock limit: a hung prompt would sit
        // parked on the fake clock forever, so a bare direct get would
        // hang the test instead of failing it.
        let prompts = prompts.clone();
        let server = server.to_owned();
        let prompt = prompt.to_owned();
        let arguments = arguments.to_owned();
        let cancel = cancel.clone();
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let output = prompts.get(&server, &prompt, &arguments, &cancel);
            done.send(output).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the get ends within {WITHIN:?}"))
    }

    fn stop(servers: crate::start::Servers) {
        let (done, stopped) = mpsc::channel();
        thread::spawn(move || {
            servers.stop();
            done.send(()).expect("collected");
        });
        stopped
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
    }

    fn requests(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("requests.log")).unwrap_or_default()
    }

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

fn greet_session() -> Session {
    let session = Session::new();
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
    let output = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn get_on_a_lazy_server_starts_it() {
    let session = greet_session();
    let first = session.start(vec![session.spec("fx")]);
    assert!(first.failed.is_empty());
    Session::stop(first.servers);
    std::fs::remove_file(session.dir.path().join("pid.txt")).expect("pid.txt");
    let second = session.start(vec![session.spec("fx")]);
    assert!(
        !session.dir.path().join("pid.txt").exists(),
        "declaring from the cache spawns nothing",
    );
    let output = Session::get(
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
    Session::stop(second.servers);
}

#[test]
fn a_missing_required_argument_is_invalid_and_never_reaches_the_server() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    let output = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn a_prompt_with_no_arguments_gets_the_text_after_its_own() {
    use contract::shapes::ContentPart;
    let session = Session::new();
    session.tools(&json!([{"name": "echo"}]));
    session.prompts(&json!([{"name": "motd", "description": "The message."}]));
    session.prompt_result(
        "motd",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Body."}}]}"#,
    );
    let started = session.start(vec![session.spec("fx")]);
    let output = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn a_json_rpc_error_fails_the_prompt() {
    use contract::ErrorCode;
    let session = Session::new();
    session.tools(&json!([{"name": "echo"}]));
    session.prompts(&json!([greet_entry(), {"name": "refused", "description": "Refused."}]));
    let started = session.start(vec![session.spec("fx")]);
    // No `prompt-refused.json`: the fixture refuses with -32602.
    let output = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn a_prompt_no_server_lists_fails_without_asking() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    let output = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn a_server_that_dies_mid_get_is_recorded_once_and_restarts_on_the_next() {
    use contract::ErrorCode;
    use contract::events::{McpServerReady, ServerFailure};
    use contract::tool::ServerRecord;
    let session = greet_session();
    session.prompt_result("greet", "exit");
    let started = session.start(vec![session.spec("fx")]);
    let first = Session::get(
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
    let second = Session::get(
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
    Session::stop(started.servers);
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
    Session::stop(started.servers);
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
    let after = Session::get(
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
    Session::stop(started.servers);
}

#[test]
fn a_dead_slot_fails_the_prompt_without_spawning() {
    use contract::ErrorCode;
    let session = greet_session();
    let started = session.start(vec![session.spec("fx")]);
    Session::stop(started.servers);
    std::fs::remove_file(session.dir.path().join("pid.txt")).expect("pid.txt");
    let output = Session::get(
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
