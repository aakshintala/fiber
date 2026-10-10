//! Parallel start, filtering, overrides and failures, through the
//! public [`start`].

use std::collections::BTreeMap;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::ServerFailure;
use contract::events::ToolSource;
use contract::shapes::Effect;
use fakes::Deadline;
use serde_json::json;

use super::{DEFAULT_STARTUP_TIMEOUT, ServerSpec, start};
use crate::effects::Hints;
use crate::test_support::{Setup, WITHIN};

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
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
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
    let mut spec = setup.spec("slow");
    spec.command = "/bin/sleep".to_owned();
    spec.args = vec!["30".to_owned()];
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(start(vec![spec], &workspace, &cache, &clock, "0.0.0"))
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
    let script = "trap '' TERM\n\"$1\" \"$2\"\nwhile :; do sleep 0.05; done\n";
    let specs = setups
        .iter()
        .enumerate()
        .map(|(index, setup)| {
            setup.tools(&json!([{"name": format!("tool{index}")}]));
            ServerSpec {
                command: "/bin/bash".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    script.to_owned(),
                    "lingering".to_owned(),
                    fakes::mcp_fixture().display().to_string(),
                    setup.dir.path().display().to_string(),
                ],
                ..setup.spec(&format!("s{index}"))
            }
        })
        .collect();
    let clock = setups[0].clock();
    let workspace = setups[0].workspace();
    let cache = workspace.join("cache");
    let (opened, started) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        opened
            .send(start(specs, &workspace, &cache, &clock, "0.0.0"))
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
    changed.args.push("changed".to_owned());
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
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
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
    let mut spec = setup.spec("slow");
    spec.command = "/bin/sleep".to_owned();
    spec.args = vec!["30".to_owned()];
    spec.required = true;
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    // Detached, not scoped: a scope joins its thread even after a
    // `recv_timeout` panic, so a hung start would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(start(vec![spec], &workspace, &cache, &clock, "0.0.0"))
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
}

#[test]
fn a_session_start_failure_carries_what_to_do() {
    let setup = Setup::new();
    let mut failing = setup.spec("bad");
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
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
    first.command = "/no/such/command".to_owned();
    first.args = Vec::new();
    first.required = true;
    let mut second = setup.spec("second");
    second.command = "/no/such/command".to_owned();
    second.args = Vec::new();
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
    let spec = ServerSpec {
        env: BTreeMap::from([("GITHUB_TOKEN".to_owned(), planted.to_owned())]),
        ..setup.spec("github")
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
