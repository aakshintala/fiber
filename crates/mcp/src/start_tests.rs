//! Parallel start, filtering, overrides and failures, through
//! [`Setup::start`] and [`start_observed_tracked`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::ServerFailure;
use contract::events::ToolSource;
use contract::events::ToolState;
use contract::shapes::ContentPart;
use contract::shapes::Effect;
use fakes::Deadline;
use serde_json::json;

use super::{DEFAULT_STARTUP_TIMEOUT, start, start_observed_tracked};
use crate::effects::Hints;
use crate::error::McpError;
use crate::test_support::{FakeImages, Setup, WITHIN};

#[test]
fn a_login_failure_has_login_advice_for_optional_and_required_servers() {
    for required in [false, true] {
        let (reason, error) = super::fail_parts(
            "docs",
            &crate::server::StartError::Login("detail".into()),
            Duration::from_secs(5),
            required,
            true,
        );
        assert_eq!(reason, ServerFailure::NotLoggedIn);
        assert_eq!(
            error,
            McpError::NotLoggedIn {
                server: "docs".to_owned(),
                required,
                detail: "detail".to_owned(),
            }
        );
    }
}
#[test]
fn two_names_share_a_url_token_but_a_changed_url_does_not() {
    let fake = fakes::ProviderServer::start_responding(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        if body["method"] == "initialize" {
            fakes::Response::status(200, json!({"jsonrpc":"2.0", "id":body["id"], "result":{"protocolVersion":"2025-06-18", "capabilities":{}}}).to_string()).header("content-type", "application/json")
        } else { fakes::Response::status(202, "") }
    }).unwrap();
    let setup = Setup::new();
    let files = Arc::new(crate::test_support::FakeTokens::default());
    let url = format!("{}/mcp", fake.url());
    crate::store_token(
        files.as_ref(),
        &url,
        &crate::Token {
            access_token: contract::Secret::new("at-1".into()),
            refresh_token: None,
            expires_at: None,
            token_endpoint: "https://auth/token".into(),
            client_id: "fiber".into(),
        },
    )
    .unwrap();
    for (name, url, expected) in [
        ("a", url.clone(), Some(fakes::fingerprint("Bearer at-1"))),
        (
            "b",
            format!("{}#x", url.replacen("http://", "HTTP://", 1)),
            Some(fakes::fingerprint("Bearer at-1")),
        ),
        ("a", format!("{}/other", fake.url()), None),
    ] {
        let offset = fake.requests().len();
        let started = setup.start(vec![setup.remote_spec_with(name, &url, files.clone())]);
        assert!(started.failed.is_empty(), "{:?}", started.failed);
        assert_eq!(
            fake.requests()[offset].header("authorization"),
            expected.as_deref()
        );
        fakes::within("remote stop", Deadline::after(WITHIN), move || {
            started.servers.stop()
        });
    }
}

#[test]
fn an_unusable_token_before_initialized_fails_the_public_start_with_login_advice() {
    let fake = fakes::ProviderServer::start_responding(|request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        fakes::Response::status(200, json!({"jsonrpc":"2.0", "id":body["id"], "result":{"protocolVersion":"2025-06-18", "capabilities":{}}}).to_string()).header("content-type", "application/json")
    }).unwrap();
    fake.hold();
    let setup = Setup::new();
    let files = Arc::new(crate::test_support::FakeTokens::default());
    let url = format!("{}/mcp", fake.url());
    crate::store_token(
        files.as_ref(),
        &url,
        &crate::Token {
            access_token: contract::Secret::new("at-1".into()),
            refresh_token: None,
            expires_at: None,
            token_endpoint: "https://auth/token".into(),
            client_id: "fiber".into(),
        },
    )
    .unwrap();
    let spec = setup.remote_spec_with("docs", &url, files.clone());
    let key = crate::token::key(&url);
    let (done, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        done.send(setup.start(vec![spec])).unwrap();
    });
    assert!(fake.await_requests(1, Deadline::after(WITHIN)));
    files.state.lock().unwrap().records.get_mut(&key).unwrap()["access_token"] = json!("bad\r\n");
    fake.release_one();
    let started = Deadline::after(WITHIN)
        .recv_or_fail(&received, "public start refuses unusable initialized token");
    worker.join().unwrap();
    assert_eq!(started.failed.len(), 1);
    assert_eq!(started.failed[0].reason, ServerFailure::NotLoggedIn);
    assert!(
        started.failed[0]
            .error
            .message
            .contains("fiber mcp login docs")
    );
    assert_eq!(fake.requests().len(), 1);
}

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
    let (held_one, release_one, reached_one) = held_spec_with_reached(&first, "one");
    let (held_two, release_two, reached_two) = held_spec_with_reached(&second, "two");
    // Detached, not scoped: a scope joins its thread even after a
    // receive panic, so a hung start would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(first.start(vec![held_one, held_two]))
            .unwrap_or(());
    });
    // Each spawn signals it reached the test's release before it
    // blocks, so the test waits for the spawns, never for a budget.
    // One deadline for the whole wait (`docs/testing.md`, "Waits and
    // timeouts"): the two receives are consecutive parts of one wait.
    let wait = Deadline::after(WITHIN);
    wait.recv_or_fail(&reached_one, "one reaches its spawn");
    wait.recv_or_fail(&reached_two, "two reaches its spawn");
    release_one.send(()).expect("the one spawn is released");
    release_two.send(()).expect("the two spawn is released");
    let started = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"));
    assert!(started.failed.is_empty());
    // `docs/mcp.md:82`: tools are sorted by name.
    assert_eq!(names(&started), ["mcp__one__zeta", "mcp__two__alpha"]);
    // The infos run parallel to the sorted tools: `start` sorts the
    // tools by name and unzips the infos from the same sorted list
    // (`crates/mcp/src/start.rs:678-682`). `docs/mcp.md` promises tool
    // order only; the infos order follows from that construction.
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

/// A spec running the fixture, held until the test sends on the
/// returned sender: the spawn waits on the test's release, never on
/// the clock, so a held server holds no clock park.
fn held_spec(setup: &Setup, name: &str) -> (super::ServerSpec, std::sync::mpsc::Sender<()>) {
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let mut spec = setup.spec(name);
    spec.stdio_mut().run = Some(super::Program {
        command: fakes::mcp_fixture().display().to_string(),
        args: vec![setup.dir.path().display().to_string()],
        guard: std::sync::Arc::new(move || {
            held.lock()
                .expect("the test holds the release")
                .recv()
                .map_err(|_| "the test dropped the release".to_owned())
        }),
    });
    (spec, release)
}

/// A held spec that also signals when its spawn reaches the test's
/// release: the guard sends on `reached` before it blocks on the
/// release, so the test waits for the spawn, never for a budget.
fn held_spec_with_reached(
    setup: &Setup,
    name: &str,
) -> (
    super::ServerSpec,
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
) {
    let (release, held) = std::sync::mpsc::channel();
    let (reached, arrived) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    let mut spec = setup.spec(name);
    spec.stdio_mut().run = Some(super::Program {
        command: fakes::mcp_fixture().display().to_string(),
        args: vec![setup.dir.path().display().to_string()],
        guard: std::sync::Arc::new(move || {
            reached
                .send(())
                .map_err(|_| "the test dropped the reached signal".to_owned())?;
            held.lock()
                .expect("the test holds the release")
                .recv()
                .map_err(|_| "the test dropped the release".to_owned())
        }),
    });
    (spec, release, arrived)
}

/// A start where one server never answers `initialize` still declares
/// the other one: the silent server fails with its deadline once the
/// test advances the fake clock to it, while the answering server waits
/// off the clock for the release. A second start whose refused server
/// reports at once but whose held server never does fails on its wall
/// guard, which names only the server still outstanding.
#[test]
fn a_silent_server_fails_its_deadline_while_the_other_declares() {
    let silent = Setup::new();
    let answering = Setup::new();
    answering.tools(&json!([{"name": "alpha"}]));
    answering.result("alpha", r#"{"content":[]}"#);
    // Silent: `sleep` answers nothing, so only the deadline ends its
    // start. The marker survives `exec` as argv[0], matching the
    // setup's guard.
    let marker = silent.dir.path().display().to_string();
    let quoted = marker.replace('\'', "'\\''");
    let mut stalled = silent.spec("quiet");
    stalled.stdio_mut().command = "/bin/bash".to_owned();
    stalled.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    let (held_two, release) = held_spec(&answering, "two");
    let fake = silent.fake.clone();
    // Detached, not scoped: a scope joins its thread even after a
    // receive panic, so a hung start would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    let specs = vec![stalled, held_two];
    std::thread::spawn(move || {
        done.send(silent.start(specs)).unwrap_or(());
    });
    let deadline = fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    // Only the silent server parks on the clock: the held server waits
    // on the test's release, so the advance ends the silent start
    // without touching the other one.
    fake.advance_when_parked(
        Some(deadline),
        None,
        DEFAULT_STARTUP_TIMEOUT,
        &Deadline::after(WITHIN),
    );
    release.send(()).expect("the held spawn is released");
    let started = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the stalled start ends within {WITHIN:?}"));
    assert_eq!(started.failed.len(), 1, "one server fails");
    assert_eq!(started.failed[0].server, "quiet");
    assert_eq!(started.failed[0].reason, ServerFailure::Deadline);
    assert_eq!(names(&started), ["mcp__two__alpha"]);
    assert_eq!(
        started
            .infos
            .iter()
            .map(|info| info.source.clone())
            .collect::<Vec<_>>(),
        vec![ToolSource::Mcp {
            server: "two".into(),
            tool: "alpha".into()
        },]
    );
    started.servers.stop();
    // The refused server reports at once, so only the held server is
    // still outstanding when the wall guard below fires: the message
    // names it and not the server that already reported. Five seconds
    // covers the thread spawns and guard round-trips with room to
    // spare under a parallel suite: neither server waits on the fake
    // clock, so no startup deadline can fire, and a mutant that changes
    // the outcome still runs out this same bound or fails faster.
    let held = Setup::new();
    let (quiet, _quiet_release) = held_spec(&held, "quiet");
    let mut refused = held.spec("two");
    {
        let __declared = declared_only("two");
        refused.stdio_mut().command = __declared.0;
        refused.stdio_mut().args = __declared.1;
    }
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    refused.stdio_mut().run = Some(run_program(&held, &gate));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        held.start_with_images_within(
            vec![quiet, refused],
            FakeImages::ok(),
            Duration::from_secs(5),
        )
    }));
    let message = match outcome {
        Ok(_) => panic!("a held start ends only on its guard"),
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "a non-string panic".to_owned()),
    };
    assert!(
        message.contains("waited 5s for the start, waiting on [quiet]"),
        "the guard names the outstanding server: {message}"
    );
    assert!(
        !message.contains("two"),
        "the guard leaves out the server that reported: {message}"
    );
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
        setup.fake.advance_when_parked(
            Some(deadline),
            None,
            DEFAULT_STARTUP_TIMEOUT,
            &Deadline::after(WITHIN),
        );
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
        watchdog.stand_down(fakes::Deadline::after(WITHIN));
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
    // Each server leaves the outstanding set when its start reports, so
    // a missed deadline names exactly the servers that did not report.
    let outstanding: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let watching = Arc::clone(&outstanding);
    let (opened, started) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        opened
            .send(start_observed_tracked(
                specs,
                &workspace,
                &artifacts,
                FakeImages::ok(),
                &cache,
                &clock,
                "0.0.0",
                None,
                Some(watching),
            ))
            .expect("collected");
    });
    let started = Deadline::after(WITHIN).recv(&started).unwrap_or_else(|_| {
        let waiting = outstanding
            .lock()
            .expect("the outstanding set")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        panic!("waited {WITHIN:?} for the start, waiting on [{waiting}]")
    });
    assert_eq!(started.failed.len(), 0);
    let grace = setups[0].fake.now() + Duration::from_millis(800);
    let (done, stopped) = std::sync::mpsc::channel();
    let servers = started.servers;
    std::thread::spawn(move || {
        servers.stop();
        done.send(()).expect("collected");
    });
    setups[0].fake.advance_when_parked_threads(
        Some(grace),
        2,
        Duration::from_millis(800),
        &Deadline::after(WITHIN),
    );
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
    setup.fake.advance_when_parked(
        Some(deadline),
        None,
        DEFAULT_STARTUP_TIMEOUT,
        &Deadline::after(WITHIN),
    );
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
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
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
                extension: false,
                name: "alpha".to_owned(),
                description: "First by server.".to_owned(),
                argument_hint: None,
                tag: "aa".to_owned(),
            },
            CommandInfo {
                extension: false,
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
    let running = crate::cache::key(command, args, &spec.stdio().env, &spec.stdio().secrets);
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
        crate::cache::key(
            &spec.stdio().command,
            &spec.stdio().args,
            &spec.stdio().env,
            &spec.stdio().secrets
        ),
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
        crate::cache::key(&stdio.command, &stdio.args, &stdio.env, &stdio.secrets),
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
    let required = spec.required;
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
    let super::Shadowed::Down { failure } = opened else {
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
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "pid {pid} to be reaped"
    );
    assert!(!fakes::pid_alive(pid), "pid {pid} to be reaped");
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

/// The tool lists a remote start caches: `tools/list` answers one `echo`.
fn remote_lists() -> Vec<crate::server_json::ListedTool> {
    vec![
        serde_json::from_value(json!({
            "name": "echo",
            "description": "Echoes.",
            "inputSchema": {"type": "object"},
        }))
        .expect("listed"),
    ]
}

/// A fake remote MCP server answering the handshake, one `echo` tool, and
/// `echo` calls with `hi`, through `start_responding` keyed on the
/// request's method.
fn remote_server() -> fakes::ProviderServer {
    fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        use serde_json::Value;
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result = |result: Value| {
            json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
        };
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => fakes::Response::status(
                200,
                result(json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]})),
            ),
            Some("tools/call") => fakes::Response::status(
                200,
                result(json!({"content": [{"type": "text", "text": "hi"}]})),
            ),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

#[test]
fn a_remote_server_starts_lists_and_caches_under_its_url() {
    let setup = Setup::new();
    let server = remote_server();
    let url = format!("{}/mcp", server.url());
    let spec = setup.remote_spec("fx", &url);
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__fx__echo"]);
    assert!(
        server
            .requests()
            .iter()
            .any(|req| req.method == "POST" && req.path == "/mcp"),
        "the start POSTs the session"
    );
    assert_eq!(
        crate::cache::read(&setup.cache(), "fx", &crate::cache::url_key(&url))
            .expect("the cache holds the remote lists")
            .tools,
        remote_lists(),
    );
}

#[test]
fn a_remote_server_with_a_matching_cache_starts_lazily() {
    let setup = Setup::new();
    let server = remote_server();
    let url = format!("{}/mcp", server.url());
    crate::cache::write(
        &setup.cache(),
        "fx",
        &crate::cache::url_key(&url),
        &crate::cache::Cached {
            tools: remote_lists(),
            prompts: Vec::new(),
            resources: false,
        },
    );
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__fx__echo"]);
    assert!(
        server.requests().is_empty(),
        "a cached server sends nothing until its first call: {:?}",
        server.requests()
    );
    let tool = setup.tool(&started, "mcp__fx__echo");
    let out = setup.run(&tool);
    assert!(
        out.error.is_none(),
        "the first call starts and runs: {:?}",
        out.error
    );
    assert!(
        server.requests().iter().any(|req| {
            serde_json::from_slice::<serde_json::Value>(&req.body)
                .ok()
                .and_then(|body| body.get("method").cloned())
                == Some(json!("initialize"))
        }),
        "the first call starts a new session: {:?}",
        server.requests()
    );
    setup.stop(started.servers);
}

#[test]
fn a_remote_start_failure_points_at_its_url() {
    // Nothing listens on port 1: the start fails, naming the `url`.
    let setup = Setup::new();
    let started = setup.start(vec![setup.remote_spec("fx", "http://127.0.0.1:1/mcp")]);
    assert_eq!(started.failed.len(), 1);
    let message = &started.failed[0].error.message;
    assert!(
        message.ends_with("Check its `url` under `mcp.servers` in your configuration."),
        "{message}"
    );
    setup.stop(started.servers);
}

#[test]
fn every_remote_tool_declares_network_whatever_its_hints() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        use serde_json::Value;
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
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => fakes::Response::status(
                200,
                result(json!({"tools": [
                    {"name": "closed", "annotations": {"openWorldHint": false}},
                    {"name": "open"},
                ]})),
            ),
            Some("tools/call") => fakes::Response::status(202, ""),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap();
    let url = format!("{}/mcp", server.url());
    let mut spec = setup.remote_spec("fx", &url);
    // The person's override drops `network` from a local tool; never from
    // a remote one.
    spec.hints.insert(
        "open".to_owned(),
        Hints {
            open_world: Some(false),
            ..Hints::default()
        },
    );
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__fx__closed", "mcp__fx__open"]);
    for (_, tool) in &started.tools {
        let effects = tool.effects(&Default::default()).expect("classifiable");
        assert_eq!(
            effects
                .declared
                .effects
                .iter()
                .filter(|effect| **effect == Effect::Network)
                .count(),
            1,
            "{} declares network exactly once",
            tool.definition().name
        );
    }
    setup.stop(started.servers);
}

#[test]
fn remote_limits_never_run_shorter_than_the_specs_deadlines() {
    // Without the max the kernel idle bound would fire before Fiber's own
    // deadline on a long silent call: each `max` arm is pinned here.
    let setup = Setup::new();
    let mut small = setup.remote_spec("fx", "http://127.0.0.1:9/mcp");
    small.call_timeout = Duration::from_secs(11);
    small.startup_timeout = Duration::from_secs(7);
    assert_eq!(
        super::limits(&small).idle(),
        net::LIMITS.idle(),
        "short deadlines keep the production idle bound"
    );
    assert_eq!(super::limits(&small).connect(), net::LIMITS.connect());
    let mut large = setup.remote_spec("fx", "http://127.0.0.1:9/mcp");
    large.call_timeout = Duration::from_secs(601);
    large.startup_timeout = Duration::from_secs(7);
    assert_eq!(
        super::limits(&large).idle(),
        Duration::from_secs(601),
        "the idle bound covers the longest deadline"
    );
    let mut startup = setup.remote_spec("fx", "http://127.0.0.1:9/mcp");
    startup.call_timeout = Duration::from_secs(7);
    startup.startup_timeout = Duration::from_secs(602);
    assert_eq!(
        super::limits(&startup).idle(),
        Duration::from_secs(602),
        "the startup deadline counts too"
    );
}

#[test]
fn a_remote_spec_debug_hides_userinfo_query_and_unparsed_urls() {
    let setup = Setup::new();
    let spec = setup.remote_spec("fx", "https://token@host:8443/mcp?key=secret");
    let printed = format!("{spec:?}");
    assert!(printed.contains("host"), "{printed}");
    assert!(printed.contains("8443"), "{printed}");
    assert!(!printed.contains("token"), "{printed}");
    assert!(!printed.contains("secret"), "{printed}");
    let broken = setup.remote_spec("fx", "not a url");
    assert!(format!("{broken:?}").contains("<unparsed>"), "{broken:?}");
}

/// A lazy spec for `name` whose cache offers resources when `resources`.
fn lazy_spec(setup: &Setup, name: &str, resources: bool) -> super::ServerSpec {
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

#[test]
fn a_lazy_slot_offers_resources_as_its_cache_says() {
    for resources in [true, false] {
        let setup = Setup::new();
        let started = setup.start(vec![lazy_spec(&setup, "fx", resources)]);
        assert!(started.failed.is_empty());
        assert!(
            !setup.spawned(),
            "declaring from the cache spawns nothing, resources: {resources}",
        );
        let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
        assert_eq!(slot.offers_resources(), resources, "resources: {resources}",);
        assert_eq!(slot.spec().name, "fx");
        setup.stop(started.servers);
    }
}

#[test]
fn adopt_copies_the_adopted_slots_and_leaves_them_reachable() {
    let setup = Setup::new();
    let a = setup.start(vec![lazy_spec(&setup, "a", false)]);
    let b = setup.start(vec![lazy_spec(&setup, "b", true)]);
    let a_slot = Arc::clone(&a.servers.slots.lock().unwrap()[0]);
    let b_slot = Arc::clone(&b.servers.slots.lock().unwrap()[0]);
    // Held across the move, so the test still sees the adopted list after.
    let b_list = Arc::clone(&b.servers.slots);
    a.servers.adopt(b.servers);
    let held = a.servers.slots.lock().unwrap();
    assert_eq!(held.len(), 2);
    assert!(Arc::ptr_eq(&held[0], &a_slot));
    assert!(Arc::ptr_eq(&held[1], &b_slot));
    drop(held);
    let kept = b_list.lock().unwrap();
    assert_eq!(kept.len(), 1, "the adopted list is left intact");
    assert!(Arc::ptr_eq(&kept[0], &b_slot));
    drop(kept);
    setup.stop(a.servers);
}

#[test]
fn nested_adoption_reaches_every_slot() {
    let setup = Setup::new();
    let a = setup.start(vec![lazy_spec(&setup, "a", false)]);
    let b = setup.start(vec![lazy_spec(&setup, "b", true)]);
    let c = setup.start(vec![lazy_spec(&setup, "c", false)]);
    let c_slot = Arc::clone(&c.servers.slots.lock().unwrap()[0]);
    let a_slot = Arc::clone(&a.servers.slots.lock().unwrap()[0]);
    let b_slot = Arc::clone(&b.servers.slots.lock().unwrap()[0]);
    // Held across both moves, so the test still sees each list after.
    let a_list = Arc::clone(&a.servers.slots);
    a.servers.adopt(b.servers);
    c.servers.adopt(a.servers);
    let reached = c.servers.slots.lock().unwrap();
    assert_eq!(reached.len(), 3);
    assert!(Arc::ptr_eq(&reached[0], &c_slot));
    assert!(Arc::ptr_eq(&reached[1], &a_slot));
    assert!(Arc::ptr_eq(&reached[2], &b_slot));
    drop(reached);
    let kept = a_list.lock().unwrap();
    assert_eq!(kept.len(), 2);
    assert!(Arc::ptr_eq(&kept[0], &a_slot));
    assert!(Arc::ptr_eq(&kept[1], &b_slot));
    drop(kept);
    assert!(b_slot.offers_resources());
    assert!(!a_slot.offers_resources());
    setup.stop(c.servers);
}

#[test]
fn the_declared_flag_starts_false_and_marks_once() {
    let setup = Setup::new();
    let plain = setup.start(vec![]);
    assert!(!plain.servers.resources_declared());
    plain.servers.mark_resources_declared();
    assert!(plain.servers.resources_declared());
    // Adopting a marked list leaves the adopter's own flag alone.
    let marked = setup.start(vec![]);
    marked.servers.mark_resources_declared();
    plain.servers.adopt(marked.servers);
    assert!(plain.servers.resources_declared());
    let fresh = setup.start(vec![]);
    assert!(!fresh.servers.resources_declared());
    fresh.servers.adopt(plain.servers);
    assert!(
        !fresh.servers.resources_declared(),
        "adopting a marked list never touches the adopter's flag",
    );
    setup.stop(fresh.servers);
}

#[test]
fn mcp_resources_is_declared_only_when_a_server_offers_resources() {
    // One server without resources declares no `mcp_resources`.
    let setup = Setup::new();
    let without = setup.start(vec![lazy_spec(&setup, "fx", false)]);
    assert!(without.failed.is_empty());
    assert_eq!(names(&without), Vec::<String>::new());
    setup.stop(without.servers);
    // A second server offering resources declares exactly one, and an
    // `all` check would miss a second registration, so the count pins it.
    let setup = Setup::new();
    let with = setup.start(vec![
        lazy_spec(&setup, "fx", false),
        lazy_spec(&setup, "docs", true),
    ]);
    assert!(with.failed.is_empty());
    assert_eq!(names(&with), ["mcp_resources"]);
    let registered: Vec<&str> = with
        .tools
        .iter()
        .filter(|(_, tool)| tool.definition().name == "mcp_resources")
        .map(|(registered_by, _)| registered_by.as_str())
        .collect();
    assert_eq!(registered, ["builtin"]);
    setup.stop(with.servers);
    // No servers declare none.
    let setup = Setup::new();
    let empty = setup.start(vec![]);
    assert_eq!(names(&empty), Vec::<String>::new());
    setup.stop(empty.servers);
}

#[test]
fn the_resources_info_is_a_deferred_builtin_row() {
    let setup = Setup::new();
    let started = setup.start(vec![lazy_spec(&setup, "docs", true)]);
    assert!(started.failed.is_empty());
    let definition = crate::resources_definition();
    let info = started
        .infos
        .iter()
        .find(|info| info.name == "mcp_resources")
        .expect("the resources info is declared");
    assert_eq!(
        *info,
        contract::events::ToolInfo {
            name: "mcp_resources".to_owned(),
            source: ToolSource::Builtin,
            state: ToolState::Deferred,
            bytes: serde_json::to_vec(&definition)
                .expect("a definition writes")
                .len() as u64,
            tokens: None,
        }
    );
    setup.stop(started.servers);
}

/// A server tool's row is deferred, keeping its MCP source.
#[test]
fn a_server_tools_info_is_a_deferred_mcp_row() {
    let setup = Setup::new();
    setup.tools(&json!([{ "name": "echo" }]));
    setup.result("echo", r#"{"content":[]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let info = started
        .infos
        .iter()
        .find(|info| info.name == "mcp__fx__echo")
        .expect("the echo info is declared");
    assert_eq!(
        info.source,
        ToolSource::Mcp {
            server: "fx".to_owned(),
            tool: "echo".to_owned(),
        }
    );
    assert_eq!(info.state, ToolState::Deferred);
    setup.stop(started.servers);
}

/// Points `spec` at a wrapper that records `$var` in a marker file, then
/// runs the fixture with the spec's arguments: the marker holds what the
/// child's environment held. The wrapper runs as `bash -c`, never as a
/// file written and executed here (see `a_non_object_initialize_reply_`
/// `fails_the_start` in `server_tests.rs`).
fn secret_wrapper(spec: &mut super::ServerSpec, setup: &Setup, var: &str) -> std::path::PathBuf {
    let marker = setup.dir.path().join("secret-value.txt");
    let fixture = fakes::mcp_fixture()
        .display()
        .to_string()
        .replace('\'', "'\\''");
    let marker_text = marker.display().to_string().replace('\'', "'\\''");
    let script = format!("echo -n \"${var}\" > '{marker_text}'\nexec '{fixture}' \"$@\"");
    let dir = setup.dir.path().display().to_string();
    let stdio = spec.stdio_mut();
    stdio.command = "/bin/bash".to_owned();
    stdio.args = vec!["-c".to_owned(), script, "wrapper".to_owned(), dir];
    marker
}

#[test]
fn launch_resolves_secret_references_into_the_child() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let mut spec = setup.spec("fx");
    let marker = secret_wrapper(&mut spec, &setup, "SECRET_VALUE");
    let stdio = spec.stdio_mut();
    stdio
        .secrets
        .insert("SECRET_VALUE".to_owned(), "mcp.fx.SECRET_VALUE".to_owned());
    stdio.reader = Arc::new(|name| {
        assert_eq!(name, "mcp.fx.SECRET_VALUE", "the reader gets the name");
        Ok("tok-live-9f3".to_owned())
    });
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty(), "{:?}", started.failed);
    assert_eq!(names(&started), ["mcp__fx__echo"]);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("the wrapper records the value"),
        "tok-live-9f3"
    );
    setup.stop(started.servers);
}

#[test]
fn launch_reads_a_secret_at_spawn_not_at_spec_build() {
    // The reader runs at `launch`, so a secret rotated after the spec was
    // built still reaches the child with its new bytes.
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let mut spec = setup.spec("fx");
    let marker = secret_wrapper(&mut spec, &setup, "SECRET_VALUE");
    let cell = Arc::new(std::sync::Mutex::new("tok-old-9f3".to_owned()));
    let stdio = spec.stdio_mut();
    stdio
        .secrets
        .insert("SECRET_VALUE".to_owned(), "mcp.fx.SECRET_VALUE".to_owned());
    let reading = Arc::clone(&cell);
    stdio.reader = Arc::new(move |_| Ok(reading.lock().expect("the cell").clone()));
    *cell.lock().expect("the cell") = "tok-new-9f3".to_owned();
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty(), "{:?}", started.failed);
    assert_eq!(
        std::fs::read_to_string(&marker).expect("the wrapper records the value"),
        "tok-new-9f3"
    );
    setup.stop(started.servers);
}

#[test]
fn a_missing_secret_fails_the_start_with_credential_missing_and_spawns_nothing() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let mut spec = setup.spec("fx");
    let stdio = spec.stdio_mut();
    stdio
        .secrets
        .insert("SECRET_VALUE".to_owned(), "mcp.fx.SECRET_VALUE".to_owned());
    stdio.reader = Arc::new(|_| Err(crate::SecretError::Missing));
    let started = setup.start(vec![spec]);
    assert!(!setup.spawned(), "no process starts without its secret");
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert_eq!(failure.server, "fx");
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert_eq!(failure.error.code, ErrorCode::CredentialMissing);
    let message = &failure.error.message;
    for words in [
        "credentials/mcp.fx.SECRET_VALUE",
        "fiber mcp remove fx",
        "fiber mcp add fx",
        "--secret SECRET_VALUE",
    ] {
        assert!(message.contains(words), "{message}");
    }
    setup.stop(started.servers);
}

#[test]
fn an_unreadable_secret_fails_the_start_with_credential_failed() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let mut spec = setup.spec("fx");
    let stdio = spec.stdio_mut();
    stdio
        .secrets
        .insert("SECRET_VALUE".to_owned(), "mcp.fx.SECRET_VALUE".to_owned());
    stdio.reader = Arc::new(|_| Err(crate::SecretError::Unreadable("denied".into())));
    let started = setup.start(vec![spec]);
    assert!(!setup.spawned(), "no process starts without its secret");
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert_eq!(failure.error.code, ErrorCode::CredentialFailed);
    let message = &failure.error.message;
    assert!(
        message.contains("credentials/mcp.fx.SECRET_VALUE"),
        "{message}"
    );
    assert!(message.contains("denied"), "{message}");
    setup.stop(started.servers);
}

#[test]
fn a_spec_debug_holds_no_secret_value() {
    let setup = Setup::new();
    let mut spec = setup.spec("fx");
    let stdio = spec.stdio_mut();
    stdio.env.insert("PLAIN".to_owned(), "plain-9f3".to_owned());
    stdio
        .secrets
        .insert("SECRET_VALUE".to_owned(), "mcp.fx.SECRET_VALUE".to_owned());
    stdio.reader = Arc::new(|_| Ok("tok-live-9f3".to_owned()));
    // Resolve once, as `launch` would, so any held value could leak.
    let _ = (stdio.reader)("mcp.fx.SECRET_VALUE").expect("the test secret reads");
    let printed = format!("{:?}", spec);
    for secret in ["tok-live-9f3", "plain-9f3"] {
        assert!(!printed.contains(secret), "{printed}");
    }
    assert!(printed.contains("SECRET_VALUE"), "{printed}");
}

#[test]
fn the_key_follows_references_never_values() {
    let setup = Setup::new();
    let mut rotated = setup.spec("fx");
    rotated
        .stdio_mut()
        .secrets
        .insert("S".to_owned(), "mcp.fx.S".to_owned());
    rotated.stdio_mut().reader = Arc::new(|_| Ok("tok-old-9f3".to_owned()));
    let mut fresh = rotated.clone();
    fresh.stdio_mut().reader = Arc::new(|_| Ok("tok-new-9f3".to_owned()));
    assert_eq!(rotated.key(), fresh.key(), "a rotation keeps the cache");
    fresh
        .stdio_mut()
        .secrets
        .insert("S".to_owned(), "mcp.fx.OTHER".to_owned());
    assert_ne!(rotated.key(), fresh.key(), "a changed reference misses");
}

#[test]
fn a_missing_secret_names_the_file_and_the_fix() {
    for required in [false, true] {
        let (reason, error) = super::fail_parts(
            "docs",
            &crate::server::StartError::SecretMissing {
                name: "mcp.docs.TOKEN".to_owned(),
            },
            Duration::from_secs(5),
            required,
            false,
        );
        assert_eq!(reason, ServerFailure::StartFailed);
        assert_eq!(
            error,
            McpError::SecretMissing {
                server: "docs".to_owned(),
                required,
                name: "mcp.docs.TOKEN".to_owned(),
            }
        );
    }
}

#[test]
fn an_unreadable_secret_names_the_file_without_its_value() {
    let (reason, error) = super::fail_parts(
        "docs",
        &crate::server::StartError::SecretUnreadable {
            name: "mcp.docs.TOKEN".to_owned(),
            why: "denied".to_owned(),
        },
        Duration::from_secs(5),
        false,
        false,
    );
    assert_eq!(reason, ServerFailure::StartFailed);
    assert_eq!(
        error,
        McpError::SecretUnreadable {
            server: "docs".to_owned(),
            required: false,
            name: "mcp.docs.TOKEN".to_owned(),
            why: "denied".to_owned(),
        }
    );
}

#[test]
fn running_names_only_the_servers_up_now() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let up = setup.spec("up");
    let mut down = setup.spec("down");
    down.transport = crate::Transport::Stdio(crate::StdioSpec {
        command: "/no/such/command".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        reader: std::sync::Arc::new(|_| Err(crate::SecretError::Missing)),
        run: None,
    });
    let lazy = lazy_spec(&setup, "lazy", false);
    let started = setup.start(vec![up, down, lazy]);
    assert_eq!(started.failed.len(), 1, "{:?}", started.failed);
    assert!(started.servers.running("up"));
    assert!(started.servers.has_slot("up"));
    assert!(!started.servers.running("down"));
    // A failed start leaves no slot: only a fresh start adopts one.
    assert!(!started.servers.has_slot("down"));
    // Declared from its cache and never started: not running.
    assert!(!started.servers.running("lazy"));
    assert!(!started.servers.running("elsewhere"));
    setup.stop(started.servers);
}

#[test]
fn restart_serves_the_slot_again_and_reports_what_changed() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let up = setup.spec("up");
    let mut down = setup.spec("down");
    down.transport = crate::Transport::Stdio(crate::StdioSpec {
        command: "/no/such/command".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        reader: std::sync::Arc::new(|_| Err(crate::SecretError::Missing)),
        run: None,
    });
    let lazy = lazy_spec(&setup, "lazy", false);
    let started = setup.start(vec![up, down, lazy]);
    assert_eq!(started.failed.len(), 1, "{:?}", started.failed);
    let servers = started.servers;
    let images: std::sync::Arc<dyn contract::images::Images> = FakeImages::ok();
    let artifacts = setup.artifacts();

    // A healthy running server restarts to a no-op: its tools stand, with
    // no records.
    let restarted = super::restart(&servers, "up", &artifacts, &images).expect("a slot names up");
    assert!(restarted.records.is_empty(), "{:?}", restarted.records);
    assert!(restarted.removed.is_empty(), "{:?}", restarted.removed);
    assert_eq!(restarted.used.len(), 1);
    assert!(servers.running("up"));

    // A failed start leaves no slot: nothing restarts until the hub
    // adopts a fresh attempt.
    assert!(
        super::restart(&servers, "down", &artifacts, &images).is_none(),
        "a failed start leaves no slot"
    );
    assert!(!servers.running("down"));

    // A cached server starts on its restart: its tools are declared.
    let restarted =
        super::restart(&servers, "lazy", &artifacts, &images).expect("a slot names lazy");
    assert!(restarted.records.is_empty(), "{:?}", restarted.records);
    assert_eq!(restarted.used.len(), 1);
    assert!(servers.running("lazy"));

    assert!(super::restart(&servers, "elsewhere", &artifacts, &images).is_none());
    setup.stop(servers);
}

#[test]
fn a_second_restart_mid_start_coalesces_on_the_first_attempt() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let dir = setup.dir.path().to_path_buf();
    let spawns = dir.join("spawns");
    setup.write(
        "flaky.sh",
        &format!(
            "#!/bin/bash\nprintf 'spawn\\n' >> \"{}\"\nexec \"{}\" \"$1\"\n",
            spawns.display(),
            fakes::mcp_fixture().display(),
        ),
    );
    let mut spec = setup.spec("flaky");
    spec.transport = super::Transport::Stdio(super::StdioSpec {
        command: "/bin/bash".into(),
        args: vec![
            dir.join("flaky.sh").display().to_string(),
            dir.display().to_string(),
        ],
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        reader: Arc::new(|_| Err(crate::SecretError::Missing)),
        run: None,
    });
    crate::cache::write(
        &setup.cache(),
        "flaky",
        &spec.key(),
        &crate::cache::Cached {
            tools: Vec::new(),
            prompts: Vec::new(),
            resources: false,
        },
    );
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty(), "{:?}", started.failed);
    assert!(!setup.spawned(), "a cached server starts nothing");
    let servers = started.servers;
    let slot = servers
        .slot("flaky")
        .expect("a cached server keeps its slot");
    assert!(!servers.running("flaky"));
    let images: Arc<dyn contract::images::Images> = FakeImages::ok();
    let artifacts = setup.artifacts();
    let (reserved_tx, reserved) = std::sync::mpsc::channel::<()>();
    let (go_tx, go) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    slot.on_after_reserve(Box::new(move || {
        reserved_tx.send(()).unwrap();
        go.recv().unwrap();
    }));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| super::restart(&servers, "flaky", &artifacts, &images));
        // The first attempt holds the lease before the second starts,
        // so the second waits on it instead of spawning beside it.
        reserved.recv().unwrap();
        let second = scope.spawn(|| {
            started_tx.send(()).unwrap();
            super::restart(&servers, "flaky", &artifacts, &images)
        });
        started_rx.recv().unwrap();
        go_tx.send(()).unwrap();
        let first = first.join().unwrap().expect("a slot names flaky");
        let second = second.join().unwrap().expect("a slot names flaky");
        // A cached start records nothing either way: both calls share
        // the one outcome, and only one attempt ran.
        assert!(first.records.is_empty(), "{:?}", first.records);
        assert!(second.records.is_empty(), "{:?}", second.records);
        assert_eq!(first.used.len(), 1);
        assert_eq!(second.used.len(), 1);
        assert!(servers.running("flaky"));
    });
    assert_eq!(
        std::fs::read_to_string(&spawns).unwrap(),
        "spawn\n",
        "one spawn serves both restarts"
    );
    setup.stop(servers);
}

// Reload, round 23: the resources marker follows every set.

#[test]
fn every_start_helper_warms_the_fixture_before_its_deadline() {
    // The first exec of a freshly linked binary can stall for seconds on
    // macOS, so every helper that starts the fixture warms it through the
    // shared once-per-process warm before any deadline-bound receive.
    // This pins only the changed helper: it fails when the `warm_up` call
    // is removed or when a helper stops warming first.
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/test_support.rs"))
            .expect("test_support.rs");
    let warmer = source.find("fn warm_once").expect("warm_once");
    let warmed = source.find("impl Setup").expect("impl Setup");
    let warm_up = source.find("fakes::warm_up").expect("warm_up");
    assert!(warmer < warm_up, "warm_once runs fakes::warm_up");
    assert!(warm_up < warmed, "warm_once runs fakes::warm_up");
    // Each helper's own wait: `start_expect` funnels through
    // `start_result`, so its warm must precede that call.
    for (helper, wait) in [
        ("start_with_images_within", "Deadline::after"),
        ("start_expect", "Self::start_result"),
        ("start_result", "Deadline::after"),
    ] {
        let defined = source
            .find(&format!("fn {helper}"))
            .unwrap_or_else(|| panic!("{helper}"));
        let warm = source
            .match_indices("warm_once(&fakes")
            .map(|(at, _)| at)
            .find(|&at| at > defined)
            .unwrap_or_else(|| panic!("{helper} warms"));
        let deadline = source
            .match_indices(wait)
            .map(|(at, _)| at)
            .find(|&at| at > defined)
            .unwrap_or_else(|| panic!("{helper} waits"));
        assert!(warm < deadline, "{helper} warms before its deadline");
    }
}

#[test]
fn set_resources_declared_aligns_the_marker_both_ways() {
    // Bites the `set_resources_declared`→`()` mutant: the marker follows
    // the last set, both ways, so the install's alignment is visible to
    // the next approval.
    let setup = Setup::new();
    let started = setup.start(vec![]);
    assert!(!started.servers.resources_declared());
    started.servers.set_resources_declared(true);
    assert!(
        started.servers.resources_declared(),
        "setting the marker declares it"
    );
    started.servers.set_resources_declared(false);
    assert!(
        !started.servers.resources_declared(),
        "clearing the marker removes it"
    );
    setup.stop(started.servers);
}
