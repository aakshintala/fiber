//! The per-server slot through the public [`start`]: the first call to a
//! cached server starts it, later calls reuse it, a failed start dies once,
//! a removed tool fails without a call, and the cache follows the live list.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, ServerFailure};
use contract::tool::ServerRecord;
use fakes::Deadline;
use serde_json::json;

use crate::error::McpError;
use crate::server_json::ListedTool;
use crate::slot::{Relisted, Run, Served, Slot, State};
use crate::start::{DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Started};
use crate::test_support::FakeImages;
use crate::test_support::{Setup, WITHIN, await_until};
use crate::{SyncRow, classify_sync, commit_sync, stage_sync};

impl Setup {
    /// Writes the cache for `spec` holding `tools` and no prompts, as a
    /// first start would.
    fn write_cache(&self, spec: &ServerSpec, tools: &[ListedTool]) {
        crate::cache::write(
            &self.cache(),
            &spec.name,
            &spec.key(),
            &crate::cache::Cached {
                tools: tools.to_vec(),
                prompts: Vec::new(),
                resources: false,
            },
        );
    }

    /// Starts once to populate the cache, stops, and clears the spawn
    /// signals, so the next start declares from the cache.
    #[track_caller]
    fn populate(&self, spec: ServerSpec) {
        let started = self.start(vec![spec]);
        assert!(started.failed.is_empty());
        self.stop(started.servers);
        std::fs::remove_file(self.dir.path().join("pid.txt")).expect("pid.txt");
        let requests = self.dir.path().join("requests.log");
        if requests.exists() {
            std::fs::remove_file(requests).expect("requests.log");
        }
    }

    /// Kills the running server of `started`'s only slot and waits until its
    /// reader has seen the exit.
    #[track_caller]
    fn kill(&self, started: &Started) {
        fakes::kill_pid(self.pid(), "KILL").expect("the server dies");
        let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
        await_until("the killed server to be gone", move || {
            if let State::Running { server, .. } = &*super::lock(&slot.state) {
                server.is_gone()
            } else {
                false
            }
        });
    }

    fn cache_inode(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(self.cache().join("fx.json"))
            .expect("the cache file")
            .ino()
    }

    fn cached_names(&self) -> Vec<String> {
        crate::cache::read(
            &self.cache(),
            "fx",
            &crate::cache::key(
                &fakes::mcp_fixture().display().to_string(),
                &[self.dir.path().display().to_string()],
                &BTreeMap::new(),
                &BTreeMap::new(),
            ),
        )
        .expect("the cache holds lists")
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
    }

    fn cached_prompts(&self) -> Vec<crate::server_json::ListedPrompt> {
        crate::cache::read(
            &self.cache(),
            "fx",
            &crate::cache::key(
                &fakes::mcp_fixture().display().to_string(),
                &[self.dir.path().display().to_string()],
                &BTreeMap::new(),
                &BTreeMap::new(),
            ),
        )
        .expect("the cache holds lists")
        .prompts
    }

    fn listed(name: &str) -> Vec<ListedTool> {
        vec![serde_json::from_value(json!({"name": name})).expect("listed")]
    }
}

/// The call's one `mcp_server_failed` record, if it carries exactly that.
fn only_failed(servers: &[ServerRecord]) -> Option<McpServerFailed> {
    match servers {
        [ServerRecord::Failed(failed)] => Some(failed.clone()),
        _ => None,
    }
}

/// A call's server lines, one word each: `ready`, or the failure's reason
/// and whether Fiber will restart it.
fn lines(servers: &[ServerRecord]) -> Vec<String> {
    servers
        .iter()
        .map(|record| match record {
            ServerRecord::Failed(failed) => {
                assert_eq!(failed.server, "fx");
                assert_eq!(failed.error.code, ErrorCode::McpServerUnavailable);
                let reason = serde_json::to_value(failed.reason).expect("reason");
                format!(
                    "failed {} {}",
                    reason.as_str().expect("a string"),
                    if failed.will_restart {
                        "restart"
                    } else {
                        "final"
                    },
                )
            }
            ServerRecord::Ready(ready) => {
                assert_eq!(ready.server, "fx");
                "ready".to_owned()
            }
        })
        .collect()
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn initializes(dir: &std::path::Path) -> usize {
    let log = std::fs::read_to_string(dir.join("requests.log")).expect("requests.log");
    log.matches(r#""method":"initialize""#).count()
}

#[test]
fn live_names_hold_the_running_servers_tools() {
    // Pins `restart`'s before-set: it diffs `slot.live_names()` into
    // `removed`, so a mutant emptying the live list would drop nothing.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = started.servers.slot("fx").expect("a slot names fx");
    assert_eq!(slot.live_names(), HashSet::from(["echo".to_owned()]));
    setup.stop(started.servers);
}

#[test]
fn the_first_call_starts_the_server_and_the_second_reuses_it() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    setup.populate(setup.spec("fx"));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "declaring from the cache spawns nothing",
    );
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert!(first.error.is_none());
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "the first call starts the server",
    );
    let second = setup.run(&tool);
    assert!(second.error.is_none());
    assert_eq!(
        initializes(setup.dir.path()),
        1,
        "both calls share the one started server",
    );
    setup.stop(started.servers);
}

#[test]
fn two_concurrent_first_calls_spawn_once() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    setup.populate(setup.spec("fx"));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let (done, results) = std::sync::mpsc::channel();
    for _ in 0..2 {
        let done = done.clone();
        let tool = Arc::clone(&tool);
        std::thread::spawn(move || {
            let output = tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
    }
    drop(done);
    let wait = Deadline::after(WITHIN);
    let first = wait
        .recv(&results)
        .unwrap_or_else(|_| panic!("the first call ends within {WITHIN:?}"));
    let second = wait
        .recv(&results)
        .unwrap_or_else(|_| panic!("the second call ends within {WITHIN:?}"));
    let outputs = vec![first, second];
    assert_eq!(outputs.len(), 2);
    for output in &outputs {
        assert!(output.error.is_none());
    }
    assert_eq!(
        initializes(setup.dir.path()),
        1,
        "concurrent first calls share one start",
    );
    setup.stop(started.servers);
}

#[test]
fn a_cached_server_whose_command_fails_dies_on_the_first_call() {
    let setup = Setup::new();
    let mut spec = setup.spec("bad");
    spec.stdio_mut().command = "/no/such/command".to_owned();
    spec.stdio_mut().args = Vec::new();
    setup.write_cache(&spec, &Setup::listed("echo"));
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__bad__echo");
    let first = setup.run(&tool);
    let error = first.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert!(
        error
            .message
            .starts_with("The MCP server `bad` failed to start: "),
        "message: {}",
        error.message,
    );
    let record = only_failed(&first.servers).expect("the failed start is recorded");
    assert_eq!(record.server, "bad");
    assert_eq!(record.reason, ServerFailure::StartFailed);
    assert!(record.will_restart, "a failed first start is its one death");
    assert_eq!(record.error.code, ErrorCode::McpServerUnavailable);
    // The second call tries the one restart, which fails too: the second
    // death, recorded once, with no restart left.
    let second = setup.run(&tool);
    let again = second.error.expect("failed");
    assert_eq!(again.code, ErrorCode::McpServerUnavailable);
    let last = only_failed(&second.servers).expect("the failed restart is recorded");
    assert_eq!(last.reason, ServerFailure::StartFailed);
    assert!(!last.will_restart);
    // Every later call repeats the failure and carries no record.
    let third = setup.run(&tool);
    assert_eq!(
        third.error.map(|error| error.code),
        Some(ErrorCode::McpServerUnavailable)
    );
    assert!(third.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn a_lazy_start_that_misses_its_deadline_fails_with_deadline() {
    let setup = Setup::new();
    // Silent: `sleep` answers nothing, so only the deadline ends the
    // lazy start. Armed before the start: the marker survives `exec` as
    // argv[0], so a failure below still kills the detached group.
    let marker = setup.dir.path().display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let quoted = marker.replace('\'', "'\\''");
    let mut spec = setup.spec("slow");
    spec.stdio_mut().command = "/bin/bash".to_owned();
    spec.stdio_mut().args = vec!["-c".to_owned(), format!("exec -a '{quoted}' /bin/sleep 30")];
    setup.write_cache(&spec, &Setup::listed("echo"));
    let started = setup.start(vec![spec]);
    let tool = setup.tool(&started, "mcp__slow__echo");
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    // Detached, not scoped: a scope joins its thread even after a
    // `recv_timeout` panic, so a hung call would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    let call = Arc::clone(&tool);
    std::thread::spawn(move || {
        let output = call.run(
            &Default::default(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        );
        done.send(output).expect("collected");
    });
    setup.fake.advance_when_parked(
        Some(deadline),
        None,
        DEFAULT_STARTUP_TIMEOUT,
        &Deadline::after(WITHIN),
    );
    let output = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the call ends");
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert_eq!(
        error.message,
        "The MCP server `slow` did not answer before its startup deadline of 5000 ms. \
         Raise `startup_timeout_ms` under `mcp.servers.slow` if it needs longer.",
    );
    let record = only_failed(&output.servers).expect("the failed start is recorded");
    assert_eq!(record.reason, ServerFailure::Deadline);
    assert!(record.will_restart, "a failed first start is its one death");
    setup.stop(started.servers);
    watchdog.stand_down(WITHIN);
}

#[test]
fn a_call_to_a_removed_tool_fails_without_calling_and_updates_the_cache() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    // The cache names a tool the live server no longer lists.
    setup.write_cache(&setup.spec("fx"), &Setup::listed("gone"));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__gone");
    let output = setup.run(&tool);
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpToolRemoved);
    assert_eq!(
        error.message,
        "The MCP server `fx` no longer has the tool `gone`; it stays declared until the next session.",
    );
    assert!(output.servers.is_empty());
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(
        !log.contains(r#""method":"tools/call""#),
        "a removed tool is never called: {log}",
    );
    // The cache now holds the live list, so the next session declares it.
    let live = crate::cache::read(
        &setup.cache(),
        "fx",
        &crate::cache::key(
            &fakes::mcp_fixture().display().to_string(),
            &[setup.dir.path().display().to_string()],
            &BTreeMap::new(),
            &BTreeMap::new(),
        ),
    )
    .expect("the cache holds the live list");
    assert_eq!(
        live.tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        ["echo"],
    );
    setup.stop(started.servers);
}

#[test]
fn a_live_list_equal_to_the_cache_leaves_the_file_untouched() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo", "description": "Echoes."}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let before =
        std::fs::read(setup.cache().join("fx.json")).expect("the first start writes the cache");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let after = std::fs::read(setup.cache().join("fx.json")).expect("cache");
    assert_eq!(before, after, "an equal live list rewrites nothing");
    setup.stop(started.servers);
}

#[test]
fn stop_stops_a_lazily_started_server_and_ignores_a_never_started_one() {
    // A slot that never started: the stop spawns nothing and returns.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let quiet = setup.start(vec![setup.spec("fx")]);
    let idle = setup.tool(&quiet, "mcp__fx__echo");
    setup.stop(quiet.servers);
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "stopping a never-started slot spawns nothing",
    );
    // A call after the stop fails without spawning: the stop marked the
    // slot dead.
    let refused = setup.run(&idle);
    assert_eq!(
        refused.error.as_ref().map(|error| &error.code),
        Some(&ErrorCode::McpServerUnavailable)
    );
    assert!(refused.servers.is_empty());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "nothing spawns after the stop",
    );
    // A lazily started server: the stop ends its child.
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let pid: u32 = std::fs::read_to_string(setup.dir.path().join("pid.txt"))
        .expect("pid.txt")
        .trim()
        .parse()
        .expect("a pid");
    setup.stop(started.servers);
    assert!(fakes::pids_exit(&[pid], WITHIN), "pid {pid} to be reaped");
    assert!(!fakes::pid_alive(pid), "pid {pid} to be reaped");
}

#[test]
fn an_annotation_only_change_rewrites_the_cache() {
    // `idempotentHint` is not one of the hints Fiber reads, so the parsed
    // tools are equal either way: only the raw entries tell the live list
    // changed.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let before = std::fs::read_to_string(setup.cache().join("fx.json"))
        .expect("the first start writes the cache");
    assert!(!before.contains("idempotentHint"), "{before:?}");
    setup.tools(&json!([{"name": "echo", "annotations": {"idempotentHint": true}}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let after = std::fs::read_to_string(setup.cache().join("fx.json")).expect("cache");
    assert_ne!(
        before, after,
        "an annotation-only change rewrites the cache"
    );
    assert!(
        after.contains("idempotentHint"),
        "the cache holds the raw entry: {after:?}",
    );
    setup.stop(started.servers);
}

const HI: &str = r#"{"content":[{"type":"text","text":"hi"}]}"#;

#[test]
fn a_failed_first_start_restarts_on_the_next_call() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    std::fs::write(setup.dir.path().join("fail-start"), "").expect("fail-start");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert_eq!(code(&first), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&first.servers), ["failed start_failed restart"]);
    std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
    let second = setup.run(&tool);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert_eq!(lines(&second.servers), ["ready"]);
    assert_eq!(initializes(setup.dir.path()), 1, "the restart answered");
    // The restarted server keeps serving, with no more records.
    let third = setup.run(&tool);
    assert!(third.error.is_none());
    assert!(third.servers.is_empty());
    assert_eq!(initializes(setup.dir.path()), 1);
    setup.stop(started.servers);
}

#[test]
fn a_failed_restart_leaves_the_server_dead_without_another_spawn() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    std::fs::write(setup.dir.path().join("fail-start"), "").expect("fail-start");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert_eq!(lines(&first.servers), ["failed start_failed restart"]);
    setup.forget_spawn();
    let second = setup.run(&tool);
    assert_eq!(code(&second), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&second.servers), ["failed start_failed final"]);
    assert!(setup.spawned(), "the second call tried the restart");
    setup.forget_spawn();
    std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
    let third = setup.run(&tool);
    assert_eq!(code(&third), Some(ErrorCode::McpServerUnavailable));
    assert!(third.servers.is_empty());
    assert!(!setup.spawned(), "a dead server never spawns again");
    setup.stop(started.servers);
}

#[test]
fn a_server_killed_while_idle_restarts_once_then_stays_dead() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    setup.kill(&started);
    let first = setup.run(&tool);
    assert!(first.error.is_none(), "{:?}", first.error);
    assert_eq!(lines(&first.servers), ["failed died restart", "ready"]);
    let ServerRecord::Failed(died) = &first.servers[0] else {
        panic!("a failed record first");
    };
    assert_eq!(
        died.error.message,
        "The MCP server `fx` exited; Fiber restarts it on the next call.",
    );
    assert_eq!(initializes(setup.dir.path()), 2, "one start, one restart");
    setup.kill(&started);
    let second = setup.run(&tool);
    assert_eq!(code(&second), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&second.servers), ["failed died final"]);
    let message = "The MCP server `fx` exited again; its tools fail until the next session.";
    assert_eq!(
        second.error.as_ref().map(|e| e.message.as_str()),
        Some(message)
    );
    setup.forget_spawn();
    let third = setup.run(&tool);
    assert_eq!(code(&third), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(
        third.error.as_ref().map(|e| e.message.as_str()),
        Some(message)
    );
    assert!(third.servers.is_empty());
    assert!(!setup.spawned(), "a dead server never spawns again");
    assert_eq!(initializes(setup.dir.path()), 2);
    setup.stop(started.servers);
}

#[test]
fn a_death_mid_call_fails_the_call_and_the_next_call_restarts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "die"}, {"name": "echo"}]));
    setup.result("die", "exit");
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let die = setup.tool(&started, "mcp__fx__die");
    let echo = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&die);
    assert_eq!(code(&first), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(
        first.error.as_ref().map(|e| e.message.as_str()),
        Some("The MCP server `fx` exited; Fiber restarts it on the next call."),
    );
    assert_eq!(lines(&first.servers), ["failed died restart"]);
    let second = setup.run(&echo);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert_eq!(lines(&second.servers), ["ready"]);
    // The restart was the last: a second death mid-call is final.
    let third = setup.run(&die);
    assert_eq!(lines(&third.servers), ["failed died final"]);
    let fourth = setup.run(&echo);
    assert_eq!(code(&fourth), Some(ErrorCode::McpServerUnavailable));
    assert!(fourth.servers.is_empty());
    assert_eq!(initializes(setup.dir.path()), 2);
    setup.stop(started.servers);
}

#[test]
fn concurrent_calls_that_see_one_death_record_it_once() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__hang");
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_CALL_TIMEOUT)
        .expect("deadline");
    let (done, results) = std::sync::mpsc::channel();
    for _ in 0..2 {
        let done = done.clone();
        let tool = Arc::clone(&tool);
        std::thread::spawn(move || {
            let output = tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
    }
    drop(done);
    assert!(
        setup.fake.await_parked_count(deadline, 2, WITHIN),
        "both calls wait on the server",
    );
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    let mut outputs = Vec::new();
    let wait = Deadline::after(WITHIN);
    for _ in 0..2 {
        outputs.push(
            wait.recv(&results)
                .unwrap_or_else(|_| panic!("each call ends within {WITHIN:?}")),
        );
    }
    for output in &outputs {
        assert_eq!(code(output), Some(ErrorCode::McpServerUnavailable));
    }
    let recorded: Vec<String> = outputs
        .iter()
        .flat_map(|output| lines(&output.servers))
        .collect();
    assert_eq!(recorded, ["failed died restart"], "one death, one record");
    setup.stop(started.servers);
}

#[test]
fn nothing_spawns_after_a_stop_from_a_dead_or_running_slot() {
    // Stopped after a death, before the restart.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "die"}, {"name": "echo"}]));
    setup.result("die", "exit");
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let die = setup.tool(&started, "mcp__fx__die");
    let echo = setup.tool(&started, "mcp__fx__echo");
    assert_eq!(lines(&setup.run(&die).servers), ["failed died restart"]);
    setup.stop(started.servers);
    setup.forget_spawn();
    let after = setup.run(&echo);
    assert_eq!(code(&after), Some(ErrorCode::McpServerUnavailable));
    assert!(after.servers.is_empty());
    assert!(!setup.spawned(), "a stopped slot never restarts");
    // Stopped while running.
    let started = setup.start(vec![setup.spec("fx")]);
    let echo = setup.tool(&started, "mcp__fx__echo");
    assert!(setup.run(&echo).error.is_none());
    setup.stop(started.servers);
    setup.forget_spawn();
    let after = setup.run(&echo);
    assert_eq!(code(&after), Some(ErrorCode::McpServerUnavailable));
    assert!(after.servers.is_empty());
    assert!(!setup.spawned(), "a stopped slot never restarts");
}

#[test]
fn a_restart_listing_the_same_tools_leaves_the_cache_untouched() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let before = setup.cache_inode();
    setup.kill(&started);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert_eq!(lines(&output.servers), ["failed died restart", "ready"]);
    assert_eq!(
        setup.cache_inode(),
        before,
        "an equal list rewrites nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn a_restart_listing_other_tools_updates_the_cache_and_keeps_the_declarations() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}, {"name": "gone"}]));
    setup.result("echo", HI);
    setup.result("gone", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    setup.kill(&started);
    setup.tools(&json!([{"name": "echo"}, {"name": "added"}]));
    let gone = setup.tool(&started, "mcp__fx__gone");
    let output = setup.run(&gone);
    assert_eq!(code(&output), Some(ErrorCode::McpToolRemoved));
    assert_eq!(
        lines(&output.servers),
        ["failed died restart", "ready"],
        "a removed tool still carries the restart's records",
    );
    assert_eq!(setup.cached_names(), ["echo", "added"]);
    let names: Vec<String> = started
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect();
    assert_eq!(names, ["mcp__fx__echo", "mcp__fx__gone"]);
    let echo = setup.tool(&started, "mcp__fx__echo");
    let again = setup.run(&echo);
    assert!(again.error.is_none());
    assert!(again.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn run_rejects_its_server_after_a_concurrent_restart_replaces_it() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let old_pid = setup.pid();
    let (served, got_served) = std::sync::mpsc::channel();
    let (resume, wait_to_resume) = std::sync::mpsc::channel();
    *super::lock(&slot.run_after_serve) = Some(Box::new(move || {
        served
            .send(())
            .expect("the call reached the live-server check");
        wait_to_resume
            .recv()
            .expect("the restart releases the waiting call");
    }));
    let (done, result) = std::sync::mpsc::channel();
    let calling = Arc::clone(&slot);
    std::thread::spawn(move || {
        done.send(calling.run("echo")).expect("collected");
    });
    Deadline::after(WITHIN)
        .recv(&got_served)
        .expect("run served the original server within the wall-clock limit");

    setup.kill(&started);
    setup.tools(&json!([{"name": "replacement"}]));
    let restarted = setup.serve(&slot);
    let new_pid = setup.pid();
    resume.send(()).expect("release the waiting call");
    let outcome = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the call ends within the wall-clock limit");

    assert_ne!(
        old_pid, new_pid,
        "the concurrent restart replaced the child"
    );
    let Served::Up(_, records) = restarted else {
        panic!("the concurrent restart brings the server back");
    };
    assert_eq!(lines(&records), ["failed died restart", "ready"]);
    let Run::Failed(failed) = outcome else {
        panic!("a call using the replaced server fails as unavailable");
    };
    assert_eq!(
        failed.error,
        McpError::ServerUnavailable {
            server: "fx".to_owned()
        }
    );
    assert!(
        failed.records.is_empty(),
        "the restart's records belong to its call"
    );
    setup.stop(started.servers);
}

#[test]
fn a_late_death_of_a_replaced_server_records_nothing() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let first = {
        let state = super::lock(&slot.state);
        let State::Running { server, .. } = &*state else {
            panic!("the session started the server");
        };
        Arc::clone(server)
    };
    setup.kill(&started);
    let tool = setup.tool(&started, "mcp__fx__echo");
    assert_eq!(
        lines(&setup.run(&tool).servers),
        ["failed died restart", "ready"]
    );
    // A call still on the first server sees it gone only now: the slot
    // already runs its replacement, so nothing is recorded or changed.
    assert!(slot.died(&first).is_none());
    let after = setup.run(&tool);
    assert!(after.error.is_none(), "{:?}", after.error);
    assert!(after.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn a_changed_prompt_list_rewrites_the_cache() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    assert!(setup.cached_prompts().is_empty());
    // Only the prompt list changes: the tools are untouched.
    std::fs::write(
        setup.dir.path().join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .expect("prompts.json");
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        setup.cached_prompts(),
        vec![
            serde_json::from_value(json!({"name": "greet", "description": "Greets."}))
                .expect("prompt")
        ]
    );
    assert_eq!(setup.cached_names(), ["echo"]);
    setup.stop(started.servers);
}

#[test]
fn a_call_ended_by_the_stop_records_no_death() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__hang");
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = tool.run(
            &Default::default(),
            &fakes::CancelToken::new(),
            &fakes::Recorder::default(),
        );
        done.send(output).expect("collected");
    });
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_CALL_TIMEOUT)
        .expect("deadline");
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the call waits on its timeout",
    );
    let slot = std::sync::Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let (stopped_tx, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        slot.stop();
        stopped_tx.send(()).expect("collected");
    });
    let output = Deadline::after(WITHIN)
        .recv(&result)
        .expect("the call ends");
    Deadline::after(WITHIN)
        .recv(&stopped)
        .expect("the stop ends");
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert_eq!(
        error.message,
        "The MCP server `fx` did not start, or it has since exited."
    );
    assert!(output.servers.is_empty(), "a stop is not a death");
    setup.stop(started.servers);
}

/// A program running the fixture, refused when `gate` is false. The test
/// flips `gate` between two operations whose results it has already
/// received: ordered, never timed.
fn refusing_run(setup: &Setup, gate: &Arc<std::sync::atomic::AtomicBool>) -> crate::start::Program {
    let gate = Arc::clone(gate);
    crate::start::Program {
        command: fakes::mcp_fixture().display().to_string(),
        args: vec![setup.dir.path().display().to_string()],
        guard: Arc::new(move || {
            if gate.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("the test refused the spawn".to_owned())
            }
        }),
    }
}

fn declared_spec(
    setup: &Setup,
    name: &str,
    gate: &Arc<std::sync::atomic::AtomicBool>,
) -> ServerSpec {
    let mut spec = setup.spec(name);
    spec.stdio_mut().command = "/no/such/declared".to_owned();
    spec.stdio_mut().args = vec![format!("--declared-{name}")];
    spec.stdio_mut().run = Some(refusing_run(setup, gate));
    spec
}

#[test]
fn a_lazy_slot_spawns_its_run_on_its_first_call() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let spec = declared_spec(&setup, "fx", &gate);
    setup.populate(spec.clone());
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    assert!(!setup.spawned(), "a cached server starts on its first call");
    let tool = setup.tool(&started, "mcp__fx__echo");
    let out = setup.run(&tool);
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(setup.spawned(), "the first call spawns the run");
    setup.stop(started.servers);
}

#[test]
fn a_first_call_the_guard_refuses_fails_without_a_spawn() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let spec = declared_spec(&setup, "fx", &gate);
    setup.populate(spec.clone());
    // Declared lazily while the guard passes; the guard is not consulted
    // until the first spawn.
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    assert!(!setup.spawned());
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let out = setup.run(&tool);
    let error = out.error.expect("the refused call fails");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert!(
        error.message.contains("the test refused the spawn"),
        "message: {}",
        error.message
    );
    let record = only_failed(&out.servers).expect("the refused start is recorded");
    assert_eq!(record.reason, ServerFailure::StartFailed);
    assert!(
        record.will_restart,
        "a refused first start is its one death"
    );
    assert!(!setup.spawned(), "a refused spawn starts nothing");
    setup.stop(started.servers);
}

#[test]
fn a_restart_the_guard_refuses_leaves_the_server_dead() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "die"}, {"name": "echo"}]));
    setup.result("die", "exit");
    setup.result("echo", HI);
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let spec = declared_spec(&setup, "fx", &gate);
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    let die = setup.tool(&started, "mcp__fx__die");
    let first = setup.run(&die);
    assert_eq!(code(&first), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&first.servers), ["failed died restart"]);
    // The restart is refused: no new spawn, and the slot follows the
    // failed-restart path.
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    setup.forget_spawn();
    let echo = setup.tool(&started, "mcp__fx__echo");
    let second = setup.run(&echo);
    let error = second.error.expect("the refused restart fails");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert!(
        error.message.contains("the test refused the spawn"),
        "message: {}",
        error.message
    );
    assert_eq!(lines(&second.servers), ["failed start_failed final"]);
    assert!(!setup.spawned(), "a refused restart spawns nothing");
    let third = setup.run(&echo);
    assert_eq!(code(&third), Some(ErrorCode::McpServerUnavailable));
    assert!(third.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn a_prompt_run_the_guard_refuses_fails_without_a_spawn() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let spec = declared_spec(&setup, "fx", &gate);
    crate::cache::write(
        &setup.cache(),
        &spec.name,
        &spec.key(),
        &crate::cache::Cached {
            tools: Setup::listed("echo"),
            prompts: vec![serde_json::from_value(json!({"name": "hello"})).expect("a prompt")],
            resources: false,
        },
    );
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    assert!(!setup.spawned());
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    let out = setup.get(
        &started.prompts,
        "fx",
        "hello",
        "{}",
        &fakes::CancelToken::new(),
    );
    let error = out.error.expect("the refused prompt run fails");
    assert_eq!(error.code, ErrorCode::McpPromptFailed);
    assert!(
        error.message.contains("the test refused the spawn"),
        "message: {}",
        error.message
    );
    assert!(!setup.spawned(), "a refused prompt run spawns nothing");
    setup.stop(started.servers);
}

/// The JSON-RPC method of a recorded fake HTTP request.
fn http_method(req: &fakes::Request) -> String {
    serde_json::from_slice::<serde_json::Value>(&req.body)
        .ok()
        .and_then(|body| {
            body.get("method")
                .and_then(|method| method.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

/// How many recorded requests carry `method`.
fn http_count(server: &fakes::ProviderServer, method: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|req| http_method(req) == method)
        .count()
}

/// A fake remote MCP server: the handshake with one `echo` tool, and
/// `echo` calls answered by `calls` from the request.
fn remote_server(
    calls: impl Fn(&fakes::Request) -> fakes::Response + Send + Sync + 'static,
) -> fakes::ProviderServer {
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
            Some("tools/call") => calls(req),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

/// An `echo` answer to the request's own id with `hi`.
fn echo_call(req: &fakes::Request) -> fakes::Response {
    use serde_json::Value;
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    fakes::Response::status(
        200,
        json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "hi"}]}}).to_string(),
    )
}

/// A remote fake answering every method through the caller's closures,
/// beside `remote_server`: each list closure answers with the id-echoing
/// `result` wrapper, so no test repeats the handshake framing. The
/// `prompts` closure answers `prompts/list`; a test whose server
/// advertises no prompts passes one that panics, since the re-list never
/// asks an unadvertised section.
fn remote_responding(
    initialize: impl Fn(&dyn Fn(serde_json::Value) -> fakes::Response) -> fakes::Response
    + Send
    + Sync
    + 'static,
    tools_list: impl Fn(&dyn Fn(serde_json::Value) -> fakes::Response) -> fakes::Response
    + Send
    + Sync
    + 'static,
    prompts_list: impl Fn(&dyn Fn(serde_json::Value) -> fakes::Response) -> fakes::Response
    + Send
    + Sync
    + 'static,
    calls: impl Fn(&fakes::Request) -> fakes::Response + Send + Sync + 'static,
) -> fakes::ProviderServer {
    fakes::ProviderServer::start_responding(move |req: &fakes::Request| {
        use serde_json::Value;
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        let status = |payload: Value| fakes::Response::status(200, result(payload));
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => initialize(&status),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => tools_list(&status),
            Some("prompts/list") => prompts_list(&status),
            Some("tools/call") => calls(req),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

#[test]
fn a_mid_session_login_failure_is_a_tool_error_and_keeps_the_slot_running() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let fake = remote_server(move |request| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            fakes::Response::status(401, "secret")
        } else {
            echo_call(request)
        }
    });
    let started = setup.start(vec![
        setup.remote_spec("docs", &format!("{}/mcp", fake.url())),
    ]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__docs__echo");
    let first = setup.run(&tool);
    assert_eq!(first.error.as_ref().unwrap().code, ErrorCode::ToolError);
    assert!(
        first
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("fiber mcp login docs")
    );
    assert!(first.servers.is_empty());
    assert!(matches!(
        *super::lock(&started.servers.slots.lock().unwrap()[0].state),
        State::Running { .. }
    ));
    let second = setup.run(&tool);
    assert!(second.error.is_none());
    assert!(second.servers.is_empty());
    assert_eq!(http_count(&fake, "initialize"), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    setup.stop(started.servers);
}

#[test]
fn a_remote_death_restarts_once_then_stays_dead() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = remote_server({
        let calls = Arc::clone(&calls);
        move |req: &fakes::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                echo_call(req)
            } else {
                fakes::Response::drop_connection()
            }
        }
    });
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    // The first call's connection drops: the death, with a restart left.
    let first = setup.run(&tool);
    assert_eq!(lines(&first.servers), ["failed died restart"]);
    assert_eq!(
        first.error.as_ref().expect("failed").message,
        "The MCP server `fx` stopped answering; Fiber reconnects on the next call."
    );
    // The next call restarts with a new `initialize`, and is recorded.
    let second = setup.run(&tool);
    assert!(
        second.error.is_none(),
        "the restart runs: {:?}",
        second.error
    );
    assert_eq!(lines(&second.servers), ["ready"]);
    assert_eq!(http_count(&server, "initialize"), 2);
    // A second death leaves it dead with its tools declared.
    let third = setup.run(&tool);
    assert_eq!(lines(&third.servers), ["failed died final"]);
    assert_eq!(
        third.error.as_ref().expect("failed").message,
        "The MCP server `fx` stopped answering again; its tools fail until the next session."
    );
    let fourth = setup.run(&tool);
    assert_eq!(
        fourth.error.as_ref().expect("failed").message,
        "The MCP server `fx` stopped answering again; its tools fail until the next session."
    );
    assert!(
        fourth.servers.is_empty(),
        "a dead slot records nothing more"
    );
    assert_eq!(http_count(&server, "tools/call"), 3, "no call is replayed");
    setup.stop(started.servers);
}

#[test]
fn an_expired_remote_session_renews_without_a_record_or_a_restart() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = remote_server({
        let calls = Arc::clone(&calls);
        move |req: &fakes::Request| match calls.fetch_add(1, Ordering::SeqCst) {
            0 => fakes::Response::status(404, "ended"),
            1 => echo_call(req),
            _ => fakes::Response::drop_connection(),
        }
    });
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    // The proven session ends: the call fails, and nothing is recorded.
    let first = setup.run(&tool);
    assert_eq!(
        first.error.as_ref().expect("failed").code,
        ErrorCode::McpServerUnavailable
    );
    assert_eq!(
        first.error.as_ref().expect("failed").message,
        "The MCP server `fx` ended its session; Fiber starts a new one on the next call."
    );
    assert!(first.servers.is_empty(), "an expiry records nothing");
    // The next call starts a new session without using the restart.
    let second = setup.run(&tool);
    assert!(
        second.error.is_none(),
        "the renewal runs: {:?}",
        second.error
    );
    assert!(second.servers.is_empty(), "a renewal records no ready");
    assert_eq!(http_count(&server, "initialize"), 2);
    // The restart is still unused: the next death restarts.
    let third = setup.run(&tool);
    assert_eq!(lines(&third.servers), ["failed died restart"]);
    setup.stop(started.servers);
}

#[test]
fn an_idle_expired_server_renews_on_its_next_call_without_a_record() {
    let setup = Setup::new();
    let server = remote_server(|_| fakes::Response::status(404, "ended"));
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    // The running server, out from under its slot: a direct call sees the
    // expiry, but the slot still runs it.
    let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let live = match &*super::lock(&slot.state) {
        State::Running { server, .. } => Arc::clone(server),
        State::NotStarted { .. } | State::Down { .. } | State::Dead { .. } => {
            panic!("the server runs")
        }
    };
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let arguments = serde_json::Map::new();
        let params = crate::rpc::Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = live.call(
            "tools/call",
            &params,
            std::time::Duration::from_secs(60),
            &fakes::CancelToken::new(),
        );
        done.send(answer).expect("collected");
    });
    let answer = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the direct call answers within {WITHIN:?}"));
    assert_eq!(answer, Err(crate::server::CallError::Expired));
    // The idle check finds the expired server and renews it with no record
    // and no failed call.
    match setup.serve(&slot) {
        Served::Up(_, records) => assert!(records.is_empty(), "no record on renewal"),
        Served::Failed(failed) => panic!("the renewal serves: {:?}", failed.error),
    }
    assert_eq!(http_count(&server, "initialize"), 2);
    setup.stop(started.servers);
}

#[test]
fn a_failed_renewal_after_a_restart_leaves_the_slot_dead() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let inits = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let server = remote_responding(
        {
            let inits = Arc::clone(&inits);
            move |result: &dyn Fn(serde_json::Value) -> fakes::Response| match inits
                .fetch_add(1, Ordering::SeqCst)
            {
                0 | 1 => result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                }))
                .header("mcp-session-id", "1868a90c"),
                _ => fakes::Response::status(500, "down"),
            }
        },
        |result| {
            result(
                json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
            )
        },
        |_| panic!("an unadvertised section is never asked"),
        {
            let calls = Arc::clone(&calls);
            move |_| match calls.fetch_add(1, Ordering::SeqCst) {
                0 => fakes::Response::drop_connection(),
                _ => fakes::Response::status(404, "ended"),
            }
        },
    );
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    // The first death restarts; the renewed session expires with no
    // record; the re-initialize fails as a death with no restart left.
    let first = setup.run(&tool);
    assert_eq!(lines(&first.servers), ["failed died restart"]);
    // The restart runs and is recorded, then its call sees the expiry:
    // the expiry itself records nothing.
    let second = setup.run(&tool);
    assert_eq!(
        second.error.as_ref().expect("failed").message,
        "The MCP server `fx` ended its session; Fiber starts a new one on the next call."
    );
    assert_eq!(lines(&second.servers), ["ready"]);
    // The re-initialize fails: a death with no restart left.
    let third = setup.run(&tool);
    assert_eq!(lines(&third.servers), ["failed start_failed final"]);
    assert!(
        third
            .error
            .as_ref()
            .expect("failed")
            .message
            .contains("Check its `url` under `mcp.servers`"),
        "the renewal failure names the url: {}",
        third.error.as_ref().expect("failed").message
    );
    let fourth = setup.run(&tool);
    assert_eq!(
        fourth.error.as_ref().expect("failed").message,
        third.error.as_ref().expect("failed").message,
        "a dead slot repeats the renewal failure"
    );
    assert!(
        fourth.servers.is_empty(),
        "a dead slot records nothing more"
    );
    assert_eq!(http_count(&server, "initialize"), 3);
    assert_eq!(http_count(&server, "tools/call"), 2, "no call is replayed");
    setup.stop(started.servers);
}

#[test]
fn call_asking_forwards_the_asker_without_leaving_the_seam() {
    // `call_asking` carries the asker to the server without bypassing
    // the fault and death mapping the shared arms below keep: the
    // elicitation is asked and answered, and the call succeeds.
    let setup = Setup::with_tools(&json!([{"name": "ask"}]));
    setup.result("ask", r#"{"content":[{"type":"text","text":"done"}]}"#);
    setup.write(
        "elicit-ask.json",
        r#"{"message":"m","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}"#,
    );
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let serving = Arc::clone(&slot);
    let server = fakes::within("the serve", WITHIN, move || match serving.run("ask") {
        Run::Call(server, _) => server,
        Run::Removed(_) => panic!("the server still lists ask"),
        Run::Failed(failed) => panic!("the server runs: {}", failed.error),
    });
    let (ask, seen) = crate::elicit::tests::FakeAsk::scripted(
        "a_9",
        vec![contract::tool::Answered::Reply(
            contract::events::Answer::Text {
                text: "prod".to_owned(),
            },
        )],
    );
    let called = fakes::within("the asking call", WITHIN, move || {
        let empty = serde_json::Map::new();
        let params = crate::rpc::Named {
            arguments: &empty,
            name: "ask",
        };
        slot.call_asking(
            &server,
            crate::rpc::TOOLS_CALL,
            &params,
            DEFAULT_CALL_TIMEOUT,
            &fakes::CancelToken::new(),
            crate::elicit::Asker {
                ask: &ask,
                server: "fx",
            },
        )
        .is_ok()
    });
    assert!(called, "the asked call succeeds");
    let asked = Deadline::after(WITHIN).recv_or_fail(&seen, "the field's ask");
    assert_eq!(
        asked.action_ids,
        vec![contract::ActionId("a_9".to_owned())],
        "the asker registers its action",
    );
    // The fixture answers the call only on reading the `accept`, so the
    // result proves it is logged: no barrier.
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let accept = log
        .lines()
        .find(|line| line.contains("\"el-") && line.contains("\"action\":\"accept\""))
        .unwrap_or_else(|| panic!("the accept reaches the wire: {log}"));
    assert!(accept.contains(r#""name":"prod""#), "{accept}");
    setup.stop(started.servers);
}

// Reload, continued: the re-list, the classification, the staged sync and
// its commit. The re-list touches nothing: every assertion on shared
// state (the cache bytes, the pid, the initialize count) reads the same
// before and after, and the commit is the only writer.

/// The image child as the sync takes it: every test's fake images.
fn every(images: &std::sync::Arc<FakeImages>) -> std::sync::Arc<dyn contract::images::Images> {
    std::sync::Arc::<FakeImages>::clone(images)
}

/// One sync's owned inputs: the locals `SyncInputs` borrows, so one
/// binding carries every stage and commit call in a test.
struct OwnedInputs {
    workspace: std::path::PathBuf,
    artifacts: std::path::PathBuf,
    images: std::sync::Arc<dyn contract::images::Images>,
    cache: std::path::PathBuf,
    clock: std::sync::Arc<dyn contract::clock::Clock>,
}

impl OwnedInputs {
    fn inputs(&self) -> crate::SyncInputs<'_> {
        crate::SyncInputs {
            workspace: &self.workspace,
            artifacts: &self.artifacts,
            images: &self.images,
            cache: &self.cache,
            clock: &self.clock,
            version: "test",
        }
    }
}

impl Setup {
    /// The `fx` slot of `started`, served once so it runs.
    fn running(&self, started: &Started) -> std::sync::Arc<Slot> {
        let slot = started.servers.slot("fx").expect("the fx slot");
        self.serve(&slot);
        slot
    }

    /// The bytes of the `fx` cache file.
    fn cache_bytes(&self) -> Vec<u8> {
        std::fs::read(self.cache().join("fx.json")).expect("the cache file")
    }

    /// Stages one classification of `specs` against `servers`, on a
    /// thread: the wedge tests advance the fake clock past each bound
    /// while the stage parks. Sends the staged sync back with the servers
    /// it borrowed, so the test stops what the stage left behind.
    fn stage_on_thread(
        rows: Vec<SyncRow>,
        servers: crate::start::Servers,
        setup: &Setup,
        images: &std::sync::Arc<FakeImages>,
    ) -> mpsc::Receiver<(
        Result<crate::StagedSync, contract::shapes::Failure>,
        crate::start::Servers,
    )> {
        let owned = OwnedInputs {
            workspace: setup.workspace(),
            artifacts: setup.artifacts(),
            images: std::sync::Arc::<FakeImages>::clone(images),
            cache: setup.cache(),
            clock: setup.clock(),
        };
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let staged = stage_sync(rows, &servers, &owned.inputs());
            done.send((staged, servers)).expect("collected");
        });
        result
    }

    /// One sync's owned inputs, built from this fixture's directories.
    fn owned_inputs(&self, images: &std::sync::Arc<FakeImages>) -> OwnedInputs {
        OwnedInputs {
            workspace: self.workspace(),
            artifacts: self.artifacts(),
            images: every(images),
            cache: self.cache(),
            clock: self.clock(),
        }
    }
}

#[test]
fn relist_picks_up_added_and_dropped_tools_without_reconnecting() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "old"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    assert_eq!(initializes(setup.dir.path()), 1);
    let pid = setup.pid();
    let cache_before = setup.cache_bytes();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "plus"}]));
    let listed = match slot.relist() {
        Relisted::Up { listed } => listed,
        Relisted::Replace { .. } => panic!("a healthy server re-lists"),
    };
    let names: Vec<String> = listed.tools.iter().map(|tool| tool.name.clone()).collect();
    assert_eq!(names, vec!["echo".to_owned(), "plus".to_owned()]);
    assert_eq!(setup.pid(), pid, "the re-list reconnects nothing");
    assert_eq!(
        initializes(setup.dir.path()),
        1,
        "the re-list reconnects nothing"
    );
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the re-list installs nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_returns_fresh_prompts_while_the_cache_waits_for_commit() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    setup.prompts(&serde_json::json!([{"name": "new"}]));
    let listed = match slot.relist() {
        Relisted::Up { listed } => listed,
        Relisted::Replace { .. } => panic!("a healthy server re-lists"),
    };
    let names: Vec<String> = listed
        .prompts
        .iter()
        .map(|prompt| prompt.name.clone())
        .collect();
    assert_eq!(names, vec!["new".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the re-list installs nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_with_errored_prompts_signals_replace_without_a_cache_write() {
    // Bites the prompts/list error swallow: a JSON-RPC error from the
    // re-list's prompts page falls back to the replace path, never an
    // empty success that would rewrite the cache from a failed listing.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    setup.prompts_raw("error");
    match slot.relist() {
        Relisted::Up { .. } => panic!("an errored prompts list never re-lists empty"),
        Relisted::Replace { .. } => {}
    };
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "a failed listing writes no cache"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_with_failed_prompts_signals_replace_without_a_cache_write() {
    // Bites the prompts/list error swallow: every prompts/list error
    // shape (a login failure, an HTTP error, a JSON-RPC error) falls
    // back to the replace path, never an empty success that would
    // rewrite the cache from a failed listing.
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A login failure (401) and an HTTP error (500) over the remote
    // transport.
    for status in [401u16, 500u16] {
        let setup = Setup::new();
        let prompts_calls = Arc::new(AtomicUsize::new(0));
        let server = remote_responding(
            |result| {
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}, "prompts": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                }))
                .header("mcp-session-id", "1868a90c")
            },
            |result| {
                result(
                    json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
                )
            },
            {
                let counted = Arc::clone(&prompts_calls);
                move |result: &dyn Fn(serde_json::Value) -> fakes::Response| {
                    if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        result(json!({"prompts": [{"name": "old", "description": "Old."}]}))
                    } else {
                        fakes::Response::status(status, "secret")
                    }
                }
            },
            |_| panic!("no call answers during a re-list"),
        );
        let started = setup.start(vec![
            setup.remote_spec("fx", &format!("{}/mcp", server.url())),
        ]);
        assert!(started.failed.is_empty());
        let slot = started.servers.slot("fx").expect("the fx slot");
        let cache_before = setup.cache_bytes();
        match slot.relist() {
            Relisted::Up { .. } => panic!("a status-{status} prompts failure never re-lists empty"),
            Relisted::Replace { .. } => {}
        }
        assert_eq!(
            setup.cache_bytes(),
            cache_before,
            "a failed listing writes no cache"
        );
        setup.stop(started.servers);
    }
    // The JSON-RPC error shape rides the stdio fixture next door:
    // `relist_with_errored_prompts` pins it to the same fallback.
}

#[test]
fn relist_without_a_running_server_signals_replace_without_records() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.populate(setup.spec("fx"));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(!setup.spawned(), "the cache declares without spawning");
    let slot = started.servers.slot("fx").expect("the fx slot");
    match slot.relist() {
        Relisted::Up { .. } => panic!("a lazy slot never re-lists"),
        Relisted::Replace { records } => assert!(records.is_empty()),
    }
    setup.stop(started.servers);
}

#[test]
fn relist_on_a_dead_server_carries_its_exit_and_moves_nothing() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    setup.kill(&started);
    let records = match slot.relist() {
        Relisted::Up { .. } => panic!("a dead server never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(records.len(), 1);
    let contract::tool::ServerRecord::Failed(failed) = &records[0] else {
        panic!("the exit rides a failed record")
    };
    assert_eq!(failed.server, "fx");
    assert_eq!(failed.reason, contract::events::ServerFailure::Died);
    assert!(failed.will_restart, "a first death restarts");
    assert!(slot.is_running(), "the re-list moves no slot");
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the re-list installs nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_of_an_idle_expired_server_carries_the_same_restarting_death() {
    // Bites the `!server.is_gone() && !server.is_expired()` with-true
    // and `||` guards: an idle-expired server is already gone to the
    // re-list, so it carries the same restartable Died record a dead
    // server does; the mutants route it into the list path, where the
    // expired session reads as an empty end with no records.
    let setup = Setup::new();
    let server = remote_server(|_| fakes::Response::status(404, "ended"));
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    // The running server, out from under its slot: a direct call latches
    // the expiry, but the slot still runs it.
    let slot = Arc::clone(&started.servers.slots.lock().unwrap()[0]);
    let live = match &*super::lock(&slot.state) {
        State::Running { server, .. } => Arc::clone(server),
        State::NotStarted { .. } | State::Down { .. } | State::Dead { .. } => {
            panic!("the server runs")
        }
    };
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let arguments = serde_json::Map::new();
        let params = crate::rpc::Named {
            arguments: &arguments,
            name: "echo",
        };
        let answer = live.call(
            "tools/call",
            &params,
            std::time::Duration::from_secs(60),
            &fakes::CancelToken::new(),
        );
        done.send(answer).expect("collected");
    });
    let answer = Deadline::after(WITHIN)
        .recv(&result)
        .unwrap_or_else(|_| panic!("the direct call answers within {WITHIN:?}"));
    assert_eq!(answer, Err(crate::server::CallError::Expired));
    // The expiry is a death the re-list is the first to see: the
    // session ended out from under the slot, so the reload restarts it
    // with the same record a dead server carries.
    let records = match slot.relist() {
        Relisted::Up { .. } => panic!("an expired server never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(records.len(), 1);
    let ServerRecord::Failed(failed) = &records[0] else {
        panic!("the expiry rides a failed record")
    };
    assert_eq!(failed.server, "fx");
    assert_eq!(failed.reason, ServerFailure::Died);
    assert!(failed.will_restart, "a first death restarts");
    assert!(slot.is_running(), "the re-list moves no slot");
    setup.stop(started.servers);
}

#[test]
fn relist_dying_mid_tools_list_carries_the_same_restarting_death() {
    // Pins the list-path Gone death to the idle death it must equal: a
    // server alive at the re-list's entry that dies mid-list carries the
    // same restartable Died record. Bites the `!restarted` flip on that
    // path.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let lists = Arc::new(AtomicUsize::new(0));
    // The handshake lists once; the re-list's list drops instead.
    let server = remote_responding(
        |result| {
            result(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "remote", "version": "1"},
            }))
            .header("mcp-session-id", "1868a90c")
        },
        {
            let lists = Arc::clone(&lists);
            move |result: &dyn Fn(serde_json::Value) -> fakes::Response| {
                if lists.fetch_add(1, Ordering::SeqCst) == 0 {
                    result(
                        json!({"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object"}}]}),
                    )
                } else {
                    fakes::Response::drop_connection()
                }
            }
        },
        |_| panic!("an unadvertised section is never asked"),
        echo_call,
    );
    let url = format!("{}/mcp", server.url());
    let started = setup.start(vec![setup.remote_spec("fx", &url)]);
    assert!(started.failed.is_empty());
    let slot = setup.running(&started);
    let records = match slot.relist() {
        Relisted::Up { .. } => panic!("a death mid-list never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(http_count(&server, "tools/list"), 2);
    assert_eq!(records.len(), 1);
    let ServerRecord::Failed(failed) = &records[0] else {
        panic!("the mid-list death rides a failed record")
    };
    assert_eq!(failed.server, "fx");
    assert_eq!(failed.reason, ServerFailure::Died);
    assert!(failed.will_restart, "a first death restarts");
    assert!(slot.is_running(), "the re-list moves no slot");
    setup.stop(started.servers);
}

#[test]
fn relist_dying_mid_prompts_list_carries_the_same_restarting_death() {
    // The prompts twin of the tools mid-list death: only a missed
    // deadline, a death or an expiry falls back to the replace path, so
    // a prompts death carries the same restartable Died record. Bites
    // the `!restarted` flip on that path.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    // `prompts.json` becomes a FIFO no one writes: `prompts/list` hangs.
    std::fs::remove_file(setup.dir.path().join("prompts.json")).expect("prompts.json");
    fakes::fifo(setup.dir.path().join("prompts.json"));
    let (done, result) = mpsc::channel();
    let relisting = Arc::clone(&slot);
    std::thread::spawn(move || {
        done.send(relisting.relist()).expect("collected");
    });
    setup.await_requests("prompts/list", 2);
    setup.kill(&started);
    let records = match Deadline::after(WITHIN)
        .recv(&result)
        .expect("the re-list ends")
    {
        Relisted::Up { .. } => panic!("a death mid-list never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(records.len(), 1);
    let ServerRecord::Failed(failed) = &records[0] else {
        panic!("the mid-list death rides a failed record")
    };
    assert_eq!(failed.server, "fx");
    assert_eq!(failed.reason, ServerFailure::Died);
    assert!(failed.will_restart, "a first death restarts");
    assert!(slot.is_running(), "the re-list moves no slot");
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the re-list installs nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_past_its_deadline_carries_the_deadline_and_touches_nothing() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let deadline = setup
        .fake
        .now()
        .checked_add(crate::start::DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    // `prompts.json` becomes a FIFO no one writes: `prompts/list` hangs.
    std::fs::remove_file(setup.dir.path().join("prompts.json")).expect("prompts.json");
    fakes::fifo(setup.dir.path().join("prompts.json"));
    let cache_before = setup.cache_bytes();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(slot.relist()).expect("collected");
    });
    setup.await_requests("tools/list", 2);
    setup.fake.advance_when_parked(
        Some(deadline),
        None,
        crate::start::DEFAULT_STARTUP_TIMEOUT,
        &fakes::Deadline::after(WITHIN),
    );
    let records = match fakes::Deadline::after(WITHIN)
        .recv(&result)
        .expect("the re-list ends")
    {
        Relisted::Up { .. } => panic!("a wedged server never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(records.len(), 1);
    let contract::tool::ServerRecord::Failed(failed) = &records[0] else {
        panic!("the deadline rides a failed record")
    };
    assert_eq!(failed.reason, contract::events::ServerFailure::Deadline);
    assert!(failed.will_restart);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the re-list installs nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn relist_pages_only_until_the_bound() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let deadline = setup
        .fake
        .now()
        .checked_add(crate::start::DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    setup.write("cursor-forever", "tools/list");
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(slot.relist()).expect("collected");
    });
    setup.fake.advance_when_parked(
        Some(deadline),
        None,
        crate::start::DEFAULT_STARTUP_TIMEOUT,
        &fakes::Deadline::after(WITHIN),
    );
    let records = match fakes::Deadline::after(WITHIN)
        .recv(&result)
        .expect("the re-list ends")
    {
        Relisted::Up { .. } => panic!("an endless list never re-lists"),
        Relisted::Replace { records } => records,
    };
    assert_eq!(records.len(), 1, "the bound is the one deadline");
    let contract::tool::ServerRecord::Failed(failed) = &records[0] else {
        panic!("the deadline rides a failed record")
    };
    assert_eq!(failed.reason, contract::events::ServerFailure::Deadline);
    setup.stop(started.servers);
}

#[test]
fn relist_lists_an_advertised_empty_tool_section() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    // The handshake advertised tools with an empty list: the re-list
    // still asks, so a tool added since shows up without a restart.
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let listed = match slot.relist() {
        Relisted::Up { listed } => listed,
        Relisted::Replace { .. } => panic!("an advertised section re-lists"),
    };
    let names: Vec<String> = listed.tools.iter().map(|tool| tool.name.clone()).collect();
    assert_eq!(names, vec!["echo".to_owned()]);
    setup.stop(started.servers);
}

#[test]
fn commit_writes_no_cache_for_an_unchanged_keep() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    let cache_before = setup.cache_bytes();
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    let images = FakeImages::ok();
    let owned = setup.owned_inputs(&images);
    let staged =
        stage_sync(rows, &started.servers, &owned.inputs()).expect("an unchanged keep stages");
    let synced = commit_sync(staged, &started.servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "equal lists leave the file untouched"
    );
    setup.stop(started.servers);
}

#[test]
fn install_relisted_refuses_a_slot_whose_server_died() {
    // The install-time liveness revalidation: installing staged lists
    // on a slot whose server died keeps what the slot holds instead of
    // publishing lists its dead server never served, so the commit
    // cannot mark a dead keep as kept.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    setup.kill(&started);
    assert!(slot.is_running(), "the kill moves no slot");
    assert!(
        !slot.install_relisted(crate::cache::Cached {
            tools: vec![
                serde_json::from_value(json!({"name": "echo"})).expect("listed"),
                serde_json::from_value(json!({"name": "plus"})).expect("listed"),
            ],
            prompts: Vec::new(),
            resources: false,
        }),
        "a dead slot declines the install"
    );
    let live: Vec<String> = {
        let mut names: Vec<String> = slot.live_names().into_iter().collect();
        names.sort();
        names
    };
    assert_eq!(
        live,
        vec!["echo".to_owned()],
        "a dead slot keeps what it holds"
    );
    let listed: Vec<String> = slot
        .listed_tools()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(listed, vec!["echo".to_owned()]);
    setup.stop(started.servers);
}

#[test]
fn install_relisted_installs_on_a_live_slot() {
    // A live slot installs the re-list and answers true, so the commit's
    // cache write persists only lists a live server served.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    assert!(
        slot.install_relisted(crate::cache::Cached {
            tools: vec![
                serde_json::from_value(json!({"name": "echo"})).expect("listed"),
                serde_json::from_value(json!({"name": "plus"})).expect("listed"),
            ],
            prompts: Vec::new(),
            resources: false,
        }),
        "a live slot installs the re-list"
    );
    let live: Vec<String> = {
        let mut names: Vec<String> = slot.live_names().into_iter().collect();
        names.sort();
        names
    };
    assert_eq!(live, vec!["echo".to_owned(), "plus".to_owned()]);
    setup.stop(started.servers);
}

#[test]
fn commit_writes_a_prompts_only_change_for_an_unchanged_tool_list() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.prompts(&serde_json::json!([{"name": "old"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    setup.running(&started);
    setup.prompts(&serde_json::json!([{"name": "new"}]));
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    let images = FakeImages::ok();
    let owned = setup.owned_inputs(&images);
    let staged =
        stage_sync(rows, &started.servers, &owned.inputs()).expect("a prompts-only change stages");
    let synced = commit_sync(staged, &started.servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    let names: Vec<String> = setup
        .cached_prompts()
        .iter()
        .map(|prompt| prompt.name.clone())
        .collect();
    assert_eq!(names, vec!["new".to_owned()]);
    setup.stop(started.servers);
}

#[test]
fn commit_installs_a_keep_dead_at_install_as_listed() {
    // A keep that dies after its re-list installs its staged lists as
    // kept: the commit stages nothing and cannot fail, so the dead
    // keep's slot and cache stay exactly as the stage left them —
    // listed but not live — and the next call restarts it.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let pid = setup.pid();
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Keep { .. }]));
    let images = FakeImages::ok();
    let owned = setup.owned_inputs(&images);
    let staged = stage_sync(rows, &started.servers, &owned.inputs()).expect("a keep stages");
    let cache_before = setup.cache_bytes();
    setup.kill(&started);
    assert!(!slot.is_live(), "the kept slot died before the commit");
    let synced = commit_sync(staged, &started.servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert!(synced.servers.restarted.is_empty());
    assert!(synced.records.is_empty(), "a keep writes no records");
    assert!(synced.failed.is_empty());
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the dead keep's cache stays as the stage left it"
    );
    assert!(!slot.is_live(), "the dead keep stays listed but not live");
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the staged lists still declare");
    assert!(
        setup.run(&echo.1).error.is_none(),
        "the next call restarts the dead keep"
    );
    assert_ne!(setup.pid(), pid, "the restart spawns its own server");
    assert!(slot.is_live(), "the keep serves again");
    setup.stop(started.servers);
}

#[test]
fn commit_never_writes_a_declined_keep_payload() {
    // A keep whose server died after staging never persists the staged
    // lists: the install declines them, so the cache keeps the old bytes
    // while the staged tools still declare and the next call restarts it.
    // The staged list differs from the cache here, so an unconditional
    // write would show: equal lists would pass either way.
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = setup.running(&started);
    let cache_before = setup.cache_bytes();
    setup.tools(&serde_json::json!([{"name": "echo"}, {"name": "plus"}]));
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Keep { .. }]));
    let images = FakeImages::ok();
    let owned = setup.owned_inputs(&images);
    let staged = stage_sync(rows, &started.servers, &owned.inputs()).expect("a keep stages");
    setup.kill(&started);
    assert!(!slot.is_live(), "the kept slot died before the commit");
    let synced = commit_sync(staged, &started.servers, &owned.inputs());
    assert_eq!(synced.servers.kept, vec!["fx".to_owned()]);
    assert_eq!(
        setup.cache_bytes(),
        cache_before,
        "the declined staged lists never reach the cache"
    );
    assert!(
        synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == "mcp__fx__plus"),
        "the staged lists still declare"
    );
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the staged lists still declare");
    assert!(
        setup.run(&echo.1).error.is_none(),
        "the next call restarts the dead keep"
    );
    assert!(slot.is_live(), "the keep serves again");
    setup.stop(started.servers);
}

#[test]
fn an_optional_replacement_failure_still_writes_the_detected_death() {
    // An optional replacement whose shadow fails still records the death
    // the sync detected: the failure row carries the old deaths, and the
    // commit writes them before the failure (`docs/mcp.md`, "Each death
    // is recorded as `mcp_server_failed`").
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    setup.running(&started);
    setup.kill(&started);
    // The replacement cannot start, but the death is already detected.
    setup.write("fail-start", "");
    let rows = classify_sync(vec![setup.spec("fx")], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Replace { .. }]));
    let images = FakeImages::ok();
    let owned = setup.owned_inputs(&images);
    let staged = stage_sync(rows, &started.servers, &owned.inputs())
        .expect("an optional shadow failure stages");
    let synced = commit_sync(staged, &started.servers, &owned.inputs());
    assert_eq!(synced.servers.restarted, vec!["fx".to_owned()]);
    assert_eq!(
        synced.records.len(),
        2,
        "the death rides beside the failure"
    );
    let reasons: Vec<ServerFailure> = synced
        .records
        .iter()
        .map(|record| match record {
            ServerRecord::Failed(failed) => failed.reason,
            ServerRecord::Ready(_) => panic!("a failure installs no ready"),
        })
        .collect();
    assert_eq!(
        reasons,
        vec![ServerFailure::Died, ServerFailure::StartFailed]
    );
    assert_eq!(synced.failed.len(), 1, "the failure rides the entry");
    setup.stop(started.servers);
}

#[test]
fn commit_declares_mcp_resources_only_when_a_slot_offers_them() {
    for resources in [true, false] {
        let setup = Setup::new();
        let spec = setup.spec("fx");
        crate::cache::write(
            &setup.cache(),
            "fx",
            &spec.key(),
            &crate::cache::Cached {
                tools: Vec::new(),
                prompts: Vec::new(),
                resources,
            },
        );
        let started = setup.start(vec![spec]);
        assert!(!setup.spawned(), "the cache declares without spawning");
        let images = FakeImages::ok();
        let owned = setup.owned_inputs(&images);
        let staged =
            stage_sync(Vec::new(), &started.servers, &owned.inputs()).expect("an empty stage");
        let synced = commit_sync(staged, &started.servers, &owned.inputs());
        let offered = synced
            .tools
            .iter()
            .any(|(_, _, info)| info.name == crate::resources::RESOURCES_TOOL);
        assert_eq!(offered, resources, "resources: {resources}");
        setup.stop(started.servers);
    }
}

#[test]
fn classify_starts_a_lazy_slot_and_commit_swaps_it_for_a_live_one() {
    let setup = Setup::new();
    setup.tools(&serde_json::json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let spec = setup.spec("fx");
    let images = FakeImages::ok();
    // The first start writes the cache; stopping it leaves the cache
    // behind, so the second start declares from the cache without
    // spawning: the slot never serves.
    let first = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
    let pid = setup.pid();
    setup.stop(first.servers);
    assert!(!fakes::pid_alive(pid), "pid {pid} to be reaped");
    let started = setup.start_with_images(vec![spec.clone()], Arc::clone(&images));
    assert_eq!(setup.pid(), pid, "the lazy start spawns nothing");
    let slot = started
        .servers
        .slot("fx")
        .expect("the lazy slot is adopted");
    assert!(!slot.is_running());
    assert!(!slot.is_live());
    let rows = classify_sync(vec![spec], &started.servers);
    assert!(matches!(rows.as_slice(), [SyncRow::Start { .. }]));
    let result = Setup::stage_on_thread(rows, started.servers, &setup, &images);
    let (outcome, servers) = fakes::Deadline::after(WITHIN)
        .recv(&result)
        .expect("the stage ends");
    let staged = outcome.expect("a fresh start stages");
    let owned = setup.owned_inputs(&images);
    let synced = commit_sync(staged, &servers, &owned.inputs());
    assert_eq!(synced.servers.started, ["fx".to_owned()]);
    assert!(
        matches!(synced.records.as_slice(), [ServerRecord::Ready(_)]),
        "a fresh start reads ready, not failed"
    );
    let echo = synced
        .tools
        .iter()
        .find(|(_, _, info)| info.name == "mcp__fx__echo")
        .expect("the new server is declared");
    assert!(setup.run(&echo.1).error.is_none());
    assert!(
        servers.slot("fx").is_some_and(|slot| slot.is_live()),
        "the commit swapped the lazy slot for a live one"
    );
    setup.stop(servers);
}
