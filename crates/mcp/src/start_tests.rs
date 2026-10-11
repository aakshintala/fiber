//! Parallel start, filtering, overrides and failures, through the
//! public [`start`].

use std::collections::BTreeMap;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::ServerFailure;
use contract::events::ToolSource;
use contract::shapes::ContentPart;
use contract::shapes::Effect;
use fakes::Deadline;
use serde_json::json;

use super::{DEFAULT_STARTUP_TIMEOUT, start};
use crate::effects::Hints;
use crate::test_support::{FakeImages, Setup, WITHIN};

fn names(started: &super::Started) -> Vec<String> {
    started
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect()
}

#[test]
fn tools_of_two_servers_declare_in_one_sorted_order() {
    let first = Setup::new();
    first.tools(&json!([{"name": "zeta"}]));
    first.result("zeta", r#"{"content":[]}"#);
    let second = Setup::new();
    second.tools(&json!([{"name": "alpha"}]));
    second.result("alpha", r#"{"content":[]}"#);
    let specs = vec![first.spec("one"), second.spec("two")];
    let started = first.start(specs);
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__one__zeta", "mcp__two__alpha"]);
    assert_eq!(
        started
            .infos
            .iter()
            .map(|info| info.source.clone())
            .collect::<Vec<_>>(),
        vec![
            ToolSource::Mcp {
                server: "one".into(),
                tool: "zeta".into()
            },
            ToolSource::Mcp {
                server: "two".into(),
                tool: "alpha".into()
            },
        ]
    );
    started.servers.stop();
}

#[test]
fn enabled_declares_only_those_tools() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "keep"}, {"name": "drop"}]));
    setup.result("keep", r#"{"content":[]}"#);
    setup.result("drop", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.enabled = Some(vec!["keep".to_owned()]);
    let started = setup.start(vec![spec]);
    assert_eq!(names(&started), ["mcp__fx__keep"]);
    started.servers.stop();
}

#[test]
fn disabled_removes_those_tools() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "keep"}, {"name": "drop"}]));
    setup.result("keep", r#"{"content":[]}"#);
    setup.result("drop", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.disabled = vec!["drop".to_owned()];
    let started = setup.start(vec![spec]);
    assert_eq!(names(&started), ["mcp__fx__keep"]);
    started.servers.stop();
}

#[test]
fn enabled_and_disabled_is_enabled_minus_disabled() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "a"}, {"name": "b"}]));
    setup.result("a", r#"{"content":[]}"#);
    setup.result("b", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.enabled = Some(vec!["a".to_owned(), "b".to_owned()]);
    spec.disabled = vec!["b".to_owned()];
    let started = setup.start(vec![spec]);
    assert_eq!(names(&started), ["mcp__fx__a"]);
    started.servers.stop();
}

#[test]
fn the_persons_hints_replace_the_servers() {
    let setup = Setup::new();
    setup.tools(&json!([{
        "name": "wipe",
        "annotations": {"readOnlyHint": true},
    }]));
    setup.result("wipe", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.hints.insert(
        "wipe".to_owned(),
        Hints {
            read_only: None,
            destructive: Some(false),
            open_world: None,
        },
    );
    let started = setup.start(vec![spec]);
    assert_eq!(names(&started), ["mcp__fx__wipe"]);
    let (_, tool) = started.tools.first().expect("one tool");
    let effects = tool.effects(&Default::default()).expect("classifiable");
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Writes, Effect::Network]
    );
    assert!(effects.declared.reversible);
    started.servers.stop();
}

#[test]
fn the_schema_passes_through_with_sorted_keys() {
    let setup = Setup::new();
    setup.tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"zebra": {"type": "string"}, "apple": {"type": "string"}},
        },
    }]));
    setup.result("echo", r#"{"content":[]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    let (_, tool) = started.tools.first().expect("one tool");
    let definition = tool.definition();
    assert_eq!(definition.description, "Echoes.");
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {"apple": {"type": "string"}, "zebra": {"type": "string"}},
        }),
    );
    started.servers.stop();
}

#[test]
fn a_long_qualified_name_is_cut() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "tool"}]));
    setup.result("tool", r#"{"content":[]}"#);
    let server = "s".repeat(crate::name::MAX_NAME_LEN);
    let mut spec = setup.spec(&server);
    spec.name = server;
    let started = setup.start(vec![spec]);
    let name = names(&started).pop().expect("one tool");
    assert_eq!(name.chars().count(), crate::name::MAX_NAME_LEN);
    assert!(name.starts_with("mcp__ssss"));
    started.servers.stop();
}

#[test]
fn a_cut_name_keeps_the_servers_own_tool_name() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "tool"}]));
    setup.result("tool", r#"{"content":[]}"#);
    let server = "s".repeat(crate::name::MAX_NAME_LEN);
    let spec = setup.spec(&server);
    let started = setup.start(vec![spec]);
    let info = started.infos.first().expect("one tool");
    assert_eq!(info.name.chars().count(), crate::name::MAX_NAME_LEN);
    assert_eq!(
        info.source,
        ToolSource::Mcp {
            server,
            tool: "tool".to_owned()
        }
    );
    started.servers.stop();
}

#[test]
fn a_failing_command_leaves_the_other_servers_tools_declared() {
    let good = Setup::new();
    good.tools(&json!([{"name": "echo"}]));
    good.result("echo", r#"{"content":[]}"#);
    let bad = Setup::new();
    let mut failing = bad.spec("bad");
    failing.stdio_mut().command = "/no/such/command".to_owned();
    failing.stdio_mut().args = Vec::new();
    let started = good.start(vec![good.spec("good"), failing]);
    assert_eq!(names(&started), ["mcp__good__echo"]);
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert_eq!(failure.server, "bad");
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert!(!failure.will_restart);
    assert_eq!(failure.error.code, ErrorCode::McpServerUnavailable);
    started.servers.stop();
}

#[test]
fn a_server_that_misses_its_deadline_is_left_out() {
    let setup = Setup::new();
    // Silent: `sleep` answers nothing, so only the deadline ends the
    // start. Armed before the start: the marker survives `exec` as
    // argv[0], so a failure below still kills the detached group.
    let marker = setup.dir.path().display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let quoted = marker.replace('\'', "'\\''");
    let mut spec = setup.spec("slow");
    spec.stdio_mut().command = "/bin/bash".to_owned();
    spec.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(start(
                vec![spec],
                &workspace,
                &artifacts,
                FakeImages::ok(),
                &cache,
                &clock,
                "0.0.0",
            ))
            .expect("collected");
        });
        assert!(
            setup.fake.await_parked(deadline, WITHIN),
            "the start waits on the startup deadline",
        );
        setup.fake.advance(DEFAULT_STARTUP_TIMEOUT);
        let started = Deadline::after(WITHIN)
            .recv(&result)
            .unwrap_or_else(|_| panic!("the failed start ends within {WITHIN:?}"));
        assert!(started.tools.is_empty());
        assert_eq!(started.failed.len(), 1);
        assert_eq!(started.failed[0].reason, ServerFailure::Deadline);
        assert_eq!(
            started.failed[0].error.code,
            ErrorCode::McpServerUnavailable
        );
        started.servers.stop();
        watchdog.stand_down(WITHIN);
    });
}

#[test]
fn no_specs_starts_nothing() {
    let setup = Setup::new();
    let started = setup.start(Vec::new());
    assert!(started.tools.is_empty());
    assert!(started.infos.is_empty());
    assert!(started.failed.is_empty());
    started.servers.stop();
}

#[test]
fn servers_stop_at_once() {
    // Each server ignores SIGTERM and outlives the end of its input, so
    // each stop waits out the grace: both are parked on the clock together.
    let setups = [Setup::new(), Setup::new()];
    let specs = setups
        .iter()
        .enumerate()
        .map(|(index, setup)| {
            setup.tools(&json!([{"name": format!("tool{index}")}]));
            // The wrapper loops past the end of its input, ignoring
            // SIGTERM: the fixture directory rides along as its argument,
            // so each setup's guard covers it.
            let script = "trap '' TERM\n\"$1\" \"$2\"\nwhile :; do sleep 0.05; done\n".to_owned();
            {
                let mut spec = setup.spec(&format!("s{index}"));
                spec.stdio_mut().command = "/bin/bash".to_owned();
                spec.stdio_mut().args = vec![
                    "-c".to_owned(),
                    script,
                    "lingering".to_owned(),
                    fakes::mcp_fixture().display().to_string(),
                    setup.dir.path().display().to_string(),
                ];
                spec
            }
        })
        .collect();
    let clock = setups[0].clock();
    let workspace = setups[0].workspace();
    let artifacts = setups[0].artifacts();
    let cache = workspace.join("cache");
    let (opened, started) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        opened
            .send(start(
                specs,
                &workspace,
                &artifacts,
                FakeImages::ok(),
                &cache,
                &clock,
                "0.0.0",
            ))
            .expect("collected");
    });
    let started = Deadline::after(WITHIN)
        .recv(&started)
        .unwrap_or_else(|_| panic!("both servers start within {WITHIN:?}"));
    assert_eq!(started.failed.len(), 0);
    let grace = setups[0].fake.now() + Duration::from_millis(800);
    let (done, stopped) = std::sync::mpsc::channel();
    let servers = started.servers;
    std::thread::spawn(move || {
        servers.stop();
        done.send(()).expect("collected");
    });
    assert!(
        setups[0].fake.await_parked_count(grace, 2, WITHIN),
        "both stops wait on the grace at once"
    );
    setups[0].fake.advance(Duration::from_millis(800));
    Deadline::after(WITHIN)
        .recv(&stopped)
        .unwrap_or_else(|_| panic!("the stop returned within {WITHIN:?}"));
}

#[test]
fn a_cached_server_declares_without_spawning() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo", "description": "Echoes."}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = setup.start(vec![setup.spec("fx")]);
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    assert!(first.failed.is_empty());
    assert!(first.required_failed.is_none());
    assert!(
        setup.workspace().join("cache").join("fx.json").exists(),
        "the first start writes the cache",
    );
    setup.stop(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    // The second start shares the workspace, so it shares the cache: it
    // declares the same tools and infos and spawns nothing.
    let second = setup.start(vec![setup.spec("fx")]);
    assert_eq!(names(&second), ["mcp__fx__echo"]);
    assert_eq!(second.infos, first.infos);
    assert!(second.failed.is_empty());
    assert!(second.required_failed.is_none());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "a cached server spawns nothing before its first call",
    );
    setup.stop(second.servers);
}

#[test]
fn a_changed_arg_misses_the_cache_and_spawns() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = setup.start(vec![setup.spec("fx")]);
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    setup.stop(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    let mut changed = setup.spec("fx");
    changed.stdio_mut().args.push("changed".to_owned());
    let second = setup.start(vec![changed]);
    assert_eq!(names(&second), ["mcp__fx__echo"]);
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "a changed declaration misses the cache and spawns",
    );
    setup.stop(second.servers);
}

#[test]
fn a_required_server_with_a_cache_starts_and_declares_live() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = setup.start(vec![setup.spec("fx")]);
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    setup.stop(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    // The live list changed since the cache was written: a required
    // server declares what the server lists now, not the cached list.
    setup.tools(&json!([{"name": "other"}]));
    setup.result("other", r#"{"content":[]}"#);
    let mut required = setup.spec("fx");
    required.required = true;
    let second = setup.start(vec![required]);
    assert_eq!(names(&second), ["mcp__fx__other"]);
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "a required server starts with the session even with a cache",
    );
    setup.stop(second.servers);
}

#[test]
fn a_required_server_that_fails_to_start_yields_required_failed() {
    let setup = Setup::new();
    let mut failing = setup.spec("bad");
    failing.stdio_mut().command = "/no/such/command".to_owned();
    failing.stdio_mut().args = Vec::new();
    failing.required = true;
    let started = setup.start(vec![failing]);
    assert!(started.tools.is_empty());
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "bad");
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert!(!failure.will_restart);
    assert_eq!(failure.error.code, ErrorCode::McpServerUnavailable);
    assert!(
        failure
            .error
            .message
            .starts_with("The required MCP server `bad` failed to start: "),
        "message: {}",
        failure.error.message,
    );
    assert!(
        failure
            .error
            .message
            .contains("Check its `command` and `args` under `mcp.servers` in your configuration.",),
        "message: {}",
        failure.error.message,
    );
    setup.stop(started.servers);
}

#[test]
fn a_required_server_that_misses_its_deadline_yields_required_failed() {
    let setup = Setup::new();
    // Silent: `sleep` answers nothing, so only the deadline ends the
    // start. Armed before the start: the marker survives `exec` as
    // argv[0], so a failure below still kills the detached group.
    let marker = setup.dir.path().display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let quoted = marker.replace('\'', "'\\''");
    let mut spec = setup.spec("slow");
    spec.stdio_mut().command = "/bin/bash".to_owned();
    spec.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    spec.required = true;
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let artifacts = setup.artifacts();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    // Detached, not scoped: a scope joins its thread even after a
    // `recv_timeout` panic, so a hung start would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(start(
            vec![spec],
            &workspace,
            &artifacts,
            FakeImages::ok(),
            &cache,
            &clock,
            "0.0.0",
        ))
        .expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the start waits on the startup deadline",
    );
    setup.fake.advance(DEFAULT_STARTUP_TIMEOUT);
    let started = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the required start ends within {WITHIN:?}"));
    assert!(started.tools.is_empty());
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "slow");
    assert_eq!(failure.reason, ServerFailure::Deadline);
    assert_eq!(
        failure.error.message,
        "The required MCP server `slow` did not answer before its startup deadline of 5000 ms. \
         Raise `startup_timeout_ms` under `mcp.servers.slow` if it needs longer.",
    );
    setup.stop(started.servers);
    watchdog.stand_down(WITHIN);
}

#[test]
fn a_session_start_failure_carries_what_to_do() {
    let setup = Setup::new();
    let mut failing = setup.spec("bad");
    failing.stdio_mut().command = "/no/such/command".to_owned();
    failing.stdio_mut().args = Vec::new();
    let started = setup.start(vec![failing]);
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert!(
        failure
            .error
            .message
            .starts_with("The MCP server `bad` failed to start: "),
        "message: {}",
        failure.error.message,
    );
    assert!(
        failure
            .error
            .message
            .contains("Check its `command` and `args` under `mcp.servers` in your configuration.",),
        "message: {}",
        failure.error.message,
    );
    setup.stop(started.servers);
}

#[test]
fn the_first_failing_required_server_in_spec_order_wins() {
    let setup = Setup::new();
    let mut first = setup.spec("first");
    first.stdio_mut().command = "/no/such/command".to_owned();
    first.stdio_mut().args = Vec::new();
    first.required = true;
    let mut second = setup.spec("second");
    second.stdio_mut().command = "/no/such/command".to_owned();
    second.stdio_mut().args = Vec::new();
    second.required = true;
    let started = setup.start(vec![first, second]);
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "first");
    setup.stop(started.servers);
}

#[test]
fn a_spec_debug_prints_no_env_value() {
    let setup = Setup::new();
    let planted = "ghp_planted4c1e9b";
    let spec = {
        let mut spec = setup.spec("github");
        spec.stdio_mut().env = BTreeMap::from([("GITHUB_TOKEN".to_owned(), planted.to_owned())]);
        spec
    };
    let printed = format!("{spec:?}");
    assert!(printed.contains("github"), "{printed}");
    assert!(printed.contains("GITHUB_TOKEN"), "{printed}");
    assert!(!printed.contains(planted), "{printed}");
}

#[test]
fn rows_list_each_servers_prompts_tagged_with_its_name_in_server_name_order() {
    use contract::events::CommandInfo;
    let first = Setup::new();
    first.tools(&json!([{"name": "echo"}]));
    first.prompts(&json!([{
        "name": "zeta",
        "description": "Last by server.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    }]));
    let second = Setup::new();
    second.tools(&json!([{"name": "echo"}]));
    second.prompts(&json!([{ "name": "alpha", "description": "First by server." }]));
    // Specs passed `zz` then `aa`: rows sort by server name.
    let started = first.start(vec![first.spec("zz"), second.spec("aa")]);
    assert!(started.failed.is_empty());
    assert_eq!(
        started.prompts.commands(),
        [
            CommandInfo {
                name: "alpha".to_owned(),
                description: "First by server.".to_owned(),
                argument_hint: None,
                tag: "aa".to_owned(),
            },
            CommandInfo {
                name: "zeta".to_owned(),
                description: "Last by server.".to_owned(),
                argument_hint: Some("<who> [tone]".to_owned()),
                tag: "zz".to_owned(),
            },
        ]
    );
    started.servers.stop();
}

#[test]
fn a_cached_server_gives_rows_without_starting() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&json!([{
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}],
    }]));
    let first = setup.start(vec![setup.spec("fx")]);
    assert!(first.failed.is_empty());
    assert_eq!(first.prompts.commands().len(), 1);
    setup.stop(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    let second = setup.start(vec![setup.spec("fx")]);
    assert!(second.failed.is_empty());
    assert_eq!(second.prompts.commands(), first.prompts.commands());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "a cached server's rows come from the cache",
    );
    setup.stop(second.servers);
}

#[test]
fn a_lazy_prompt_run_starts_the_server_and_sends_its_arguments() {
    use contract::shapes::ContentPart;
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&json!([{
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    }]));
    setup.prompt_result(
        "greet",
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Say hello to Ada, warmly."}}]}"#,
    );
    let first = setup.start(vec![setup.spec("fx")]);
    assert!(first.failed.is_empty());
    setup.stop(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    let second = setup.start(vec![setup.spec("fx")]);
    assert!(!setup.spawned(), "declaring from the cache spawns nothing",);
    let out = setup.get(
        &second.prompts,
        "fx",
        "greet",
        "Ada warm",
        &fakes::CancelToken::new(),
    );
    assert!(out.error.is_none());
    assert_eq!(
        out.content,
        [ContentPart::Text {
            text: "Say hello to Ada, warmly.".to_owned(),
        }]
    );
    assert!(setup.spawned(), "the first prompt run starts the server");
    let line = setup
        .requests()
        .lines()
        .find(|line| line.contains(r#""method":"prompts/get""#))
        .expect("a prompts/get line")
        .to_owned();
    assert!(
        line.contains(r#""arguments":{"tone":"warm","who":"Ada"}"#),
        "the get sends the named arguments: {line}",
    );
    setup.stop(second.servers);
}

/// The `Saved` line a binary result writes for `bytes` of `mime`: the same
/// content-addressed name the tool writes, so the tests pin the exact line.
fn saved_line(mime: &str, bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "Saved {mime} content, {} bytes, to artifacts/mcp-{}.bin",
        bytes.len(),
        &hex[..16],
    )
}

/// The file a `Saved` line names, under the setup's artifacts dir.
fn saved_file(setup: &Setup, line: &str) -> std::path::PathBuf {
    let name = line.split("to artifacts/").nth(1).expect("a Saved line");
    setup.artifacts().join(name)
}

#[test]
fn mixed_content_runs_through_start_and_saves_binary() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result(
        "echo",
        r#"{"content":[{"type":"text","text":"hello"},{"type":"resource_link","uri":"file:///x","name":"x","mimeType":"text/plain"},{"type":"resource","resource":{"text":"from a file","uri":"file:///x","mimeType":"text/plain"}},{"type":"resource","resource":{"blob":"aGk=","mimeType":"application/pdf"}}]}"#,
    );
    std::fs::create_dir_all(setup.artifacts()).expect("artifacts dir");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![
            ContentPart::Text {
                text: "hello".to_owned(),
            },
            ContentPart::Text {
                text: "Resource link: file:///x (name: x, type: text/plain)".to_owned(),
            },
            ContentPart::Text {
                text: "from a file".to_owned(),
            },
            ContentPart::Text {
                text: saved_line("application/pdf", b"hi"),
            },
        ],
    );
    let ContentPart::Text { text } = &output.content[3] else {
        panic!("one Saved line");
    };
    let read = std::fs::read(saved_file(&setup, text)).expect("the artifact is written");
    assert_eq!(read, b"hi");
    started.servers.stop();
}

#[test]
fn audio_data_runs_through_start_and_saves_under_its_type() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result(
        "echo",
        r#"{"content":[{"type":"audio","data":"aGk=","mimeType":"audio/mpeg"}]}"#,
    );
    std::fs::create_dir_all(setup.artifacts()).expect("artifacts dir");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: saved_line("audio/mpeg", b"hi"),
        }],
    );
    let ContentPart::Text { text } = &output.content[0] else {
        panic!("one Saved line");
    };
    let read = std::fs::read(saved_file(&setup, text)).expect("the artifact is written");
    assert_eq!(read, b"hi");
    started.servers.stop();
}

#[test]
fn audio_without_data_reports_through_start() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"audio"}]}"#);
    std::fs::create_dir_all(setup.artifacts()).expect("artifacts dir");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: "Could not save application/octet-stream content: no data".to_owned(),
        }],
    );
    started.servers.stop();
}

#[test]
fn image_results_run_through_start_and_process_once() {
    // `cmF3LWltYWdl` is `raw-image`: the bytes the fake sees.
    let expected = vec![
        ContentPart::Text {
            text: "Image: 2x3 image/png.\n".to_owned(),
        },
        ContentPart::Image {
            path: "artifacts/mcp-fake.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 2,
            height: 3,
        },
    ];
    for (name, body) in [
        (
            "image part",
            r#"{"content":[{"type":"image","data":"cmF3LWltYWdl","mimeType":"image/png"}]}"#,
        ),
        (
            "embedded image resource",
            r#"{"content":[{"type":"resource","resource":{"blob":"cmF3LWltYWdl","mimeType":"image/png"}}]}"#,
        ),
    ] {
        let setup = Setup::new();
        setup.tools(&json!([{"name": "echo"}]));
        setup.result("echo", body);
        std::fs::create_dir_all(setup.artifacts()).expect("artifacts dir");
        let images = FakeImages::ok();
        let started =
            setup.start_with_images(vec![setup.spec("fx")], std::sync::Arc::clone(&images));
        let tool = setup.tool(&started, "mcp__fx__echo");
        let output = setup.run(&tool);
        assert!(output.error.is_none(), "row: {name}");
        assert_eq!(output.content, expected, "row: {name}");
        assert_eq!(images.calls(), vec![b"raw-image".to_vec()], "row: {name}");
        assert_eq!(images.call_count(), 1, "row: {name}");
        started.servers.stop();
    }
}

#[test]
fn an_embedded_pdf_runs_through_start_and_saves_without_images() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result(
        "echo",
        r#"{"content":[{"type":"resource","resource":{"blob":"aGk=","mimeType":"application/pdf"}}]}"#,
    );
    std::fs::create_dir_all(setup.artifacts()).expect("artifacts dir");
    let images = FakeImages::ok();
    let started = setup.start_with_images(vec![setup.spec("fx")], std::sync::Arc::clone(&images));
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        output.content,
        vec![ContentPart::Text {
            text: saved_line("application/pdf", b"hi"),
        }],
    );
    assert_eq!(images.call_count(), 0);
    let ContentPart::Text { text } = &output.content[0] else {
        panic!("one Saved line");
    };
    let read = std::fs::read(saved_file(&setup, text)).expect("the artifact is written");
    assert_eq!(read, b"hi");
    started.servers.stop();
}

/// A program running the fixture on the setup's directory, refused when
/// `gate` is false. The test flips `gate` on its own thread between two
/// operations whose results it has already received: ordered, never timed.
fn run_program(
    setup: &Setup,
    gate: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> super::Program {
    let gate = std::sync::Arc::clone(gate);
    super::Program {
        command: fakes::mcp_fixture().display().to_string(),
        args: vec![setup.dir.path().display().to_string()],
        guard: std::sync::Arc::new(move || {
            if gate.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("the test refused the spawn".to_owned())
            }
        }),
    }
}

fn declared_only(name: &str) -> (String, Vec<String>) {
    (
        "/no/such/declared".to_owned(),
        vec![format!("--declared-{name}")],
    )
}

#[test]
fn a_run_starts_lists_and_caches_under_the_declaration() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut spec = setup.spec("fx");
    {
        let __declared = declared_only("fx");
        spec.stdio_mut().command = __declared.0;
        spec.stdio_mut().args = __declared.1;
    }
    spec.stdio_mut().run = Some(run_program(&setup, &gate));
    let started = setup.start(vec![spec.clone()]);
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__fx__echo"]);
    assert!(setup.spawned(), "the run spawns at session start");
    assert!(setup.cache().join("fx.json").is_file());
    let declared = spec.key();
    assert!(crate::cache::read(&setup.cache(), "fx", &declared).is_some());
    let (command, args) = spec.stdio().program();
    let running = crate::cache::key(command, args, &spec.stdio().env);
    assert_ne!(
        declared, running,
        "the pinned paths never enter the cache key"
    );
    assert!(crate::cache::read(&setup.cache(), "fx", &running).is_none());
    setup.stop(started.servers);
}

#[test]
fn a_second_start_with_a_different_run_hits_the_declaration_cache() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut spec = setup.spec("fx");
    {
        let __declared = declared_only("fx");
        spec.stdio_mut().command = __declared.0;
        spec.stdio_mut().args = __declared.1;
    }
    spec.stdio_mut().run = Some(run_program(&setup, &gate));
    let first = setup.start(vec![spec.clone()]);
    assert!(first.failed.is_empty());
    setup.stop(first.servers);
    setup.forget_spawn();
    // The same declaration with a different program: the cached list still
    // declares, lazily, and nothing spawns.
    let mut again = spec.clone();
    again.stdio_mut().run = Some(super::Program {
        command: "/no/such/other".to_owned(),
        args: Vec::new(),
        guard: std::sync::Arc::new(|| Ok(())),
    });
    let second = setup.start(vec![again]);
    assert!(second.failed.is_empty());
    assert_eq!(names(&second), ["mcp__fx__echo"]);
    assert!(!setup.spawned(), "the cache hit spawns nothing");
    setup.stop(second.servers);
}

#[test]
fn program_is_the_run_when_set_and_the_declaration_otherwise() {
    let setup = Setup::new();
    let plain = setup.spec("fx");
    let (command, args) = plain.stdio().program();
    assert_eq!(command, plain.stdio().command.as_str());
    assert_eq!(args, plain.stdio().args.as_slice());
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut run = setup.spec("fx");
    {
        let __declared = declared_only("fx");
        run.stdio_mut().command = __declared.0;
        run.stdio_mut().args = __declared.1;
    }
    run.stdio_mut().run = Some(run_program(&setup, &gate));
    let (command, args) = run.stdio().program();
    let program = run.stdio().run.as_ref().expect("a run");
    assert_eq!(command, program.command.as_str());
    assert_eq!(args, program.args.as_slice());
}

#[test]
fn a_stdio_spec_key_is_its_declaration_never_its_run() {
    // Without the dispatch the key would follow the pinned program:
    // the `run` mutant changes the key here.
    let setup = Setup::new();
    let spec = setup.spec("fx");
    assert_eq!(
        spec.key(),
        crate::cache::key(&spec.stdio().command, &spec.stdio().args, &spec.stdio().env),
        "the key is the declaration's"
    );
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut run = setup.spec("fx");
    let declared = declared_only("fx");
    run.stdio_mut().command = declared.0;
    run.stdio_mut().args = declared.1;
    run.stdio_mut().run = Some(run_program(&setup, &gate));
    let stdio = run.stdio();
    assert_eq!(
        run.key(),
        crate::cache::key(&stdio.command, &stdio.args, &stdio.env),
        "the key stays the declaration's with a run set"
    );
}

#[test]
fn a_session_start_whose_guard_refuses_reports_start_failed() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut spec = setup.spec("fx");
    {
        let __declared = declared_only("fx");
        spec.stdio_mut().command = __declared.0;
        spec.stdio_mut().args = __declared.1;
    }
    spec.stdio_mut().run = Some(run_program(&setup, &gate));
    let images: std::sync::Arc<dyn contract::images::Images> = FakeImages::ok();
    let opened = super::open(
        spec,
        &setup.workspace(),
        &setup.artifacts(),
        &images,
        &setup.cache(),
        &setup.clock(),
        "0.0.0",
    );
    let super::Opened::Down { failure, required } = opened else {
        panic!("a refused guard fails the start");
    };
    assert!(!required);
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert!(
        failure.error.message.contains("the test refused the spawn"),
        "message: {}",
        failure.error.message
    );
    assert!(!setup.spawned(), "a refused spawn starts nothing");
}

#[test]
fn adopt_then_stop_stops_the_adopted_server() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let pid = setup.pid();
    let idle = setup.start(vec![]);
    idle.servers.adopt(started.servers);
    assert_eq!(idle.servers.slots.lock().unwrap().len(), 1);
    setup.stop(idle.servers);
    setup.await_reaped(pid);
}

#[test]
fn a_program_debug_redacts_its_command_and_arguments() {
    let program = super::Program {
        command: "planted-command-secret".to_owned(),
        args: vec!["--token=planted-argument-secret".to_owned()],
        guard: std::sync::Arc::new(|| Ok(())),
    };
    let printed = format!("{program:?}");
    assert!(printed.contains("Program"), "{printed}");
    assert!(printed.contains("command: \"redacted\""), "{printed}");
    assert!(printed.contains("args: 1"), "{printed}");
    assert!(!printed.contains("planted-command-secret"), "{printed}");
    assert!(!printed.contains("planted-argument-secret"), "{printed}");
}

#[test]
fn a_spec_debug_prints_no_argument_run_or_env_value() {
    let setup = Setup::new();
    let planted_env = "ghp_planted4c1e9b";
    let planted_arg = "--token=planted7d2a";
    let planted_run = "--key=planted9f31";
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let run = super::Program {
        command: "/run/program".to_owned(),
        args: vec![planted_run.to_owned()],
        guard: std::sync::Arc::clone(&run_program(&setup, &gate).guard),
    };
    let spec = {
        let mut spec = setup.spec("github");
        spec.stdio_mut().args = vec![planted_arg.to_owned()];
        spec.stdio_mut().env =
            BTreeMap::from([("GITHUB_TOKEN".to_owned(), planted_env.to_owned())]);
        spec.stdio_mut().run = Some(run);
        spec
    };
    let printed = format!("{spec:?}");
    assert!(printed.contains("github"), "{printed}");
    assert!(!printed.contains(planted_env), "{printed}");
    assert!(!printed.contains(planted_arg), "{printed}");
    assert!(!printed.contains(planted_run), "{printed}");
    assert!(!printed.contains("/run/program"), "{printed}");
}
