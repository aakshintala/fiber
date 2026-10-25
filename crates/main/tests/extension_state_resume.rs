//! State reaches the first hook through resume, forks, rewinds and handoffs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
mod extension_harness;
mod support;

use contract::events::{
    Event, ExtensionStateSet, ExtensionStateUnset, OnFork, SessionStarted, Variables,
    VariablesSource,
};
use contract::shapes::Point;
use contract::{Seq, SessionId};
use fakes::ProviderServer;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use support::Setup;

const PARENT: &str = "s_1111111111111111";
const CHILD: &str = "s_2222222222222222";
const READ: &str = "fiber.hook('session_start',{timeout=5000,on_failure='blocking',run=function() local v=state.get('goal');state.set('seen',v==nil and 'absent' or v) end})";

fn initial(setup: &Setup, id: &str, parent: Option<Point>) -> log::Log {
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let log = log::Log::create(
        &sessions,
        SessionId(id.into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: std::fs::canonicalize(setup.workspace())
                .unwrap()
                .display()
                .to_string(),
            variables: Variables {
                path: std::env::var("PATH").unwrap_or_default(),
                names: vec!["FIBER_TEST_FAKE_KEY".into()],
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: parent,
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    log
}
fn set(log: &log::Log, key: &str, value: Value) -> Seq {
    log.append(
        &Event::ExtensionStateSet(ExtensionStateSet {
            extension: "fiber.test/goal".into(),
            key: key.into(),
            value,
            on_fork: OnFork::AtPoint,
        }),
        None,
        None,
    )
    .unwrap()
    .seq
    .unwrap()
}
fn ask(setup: &Setup, id: &str) {
    let mut command = setup.fiber(&["ask", "--resume", id, "hello"]);
    command.current_dir(setup.workspace());
    let output = support::run_to_exit(setup.deadline, "state resume process exited", command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn seen(setup: &Setup, id: &str) -> Value {
    let lines = log::read(&support::session_dir(setup, id)).unwrap();
    lines
        .iter()
        .rev()
        .find(|line| line.kind == "extension_state_set" && line.payload["key"] == "seen")
        .expect("the first hook wrote seen")
        .payload["value"]
        .clone()
}

#[test]
fn resumed_state_is_folded_before_session_start_including_unsets() {
    for removed in [false, true] {
        let setup = Setup::new();
        let server = ProviderServer::start([support::text_reply("done")]).unwrap();
        setup.provider(&server);
        setup.lua("goal", READ);
        let log = initial(&setup, PARENT, None);
        set(&log, "goal", json!({"n":3}));
        if removed {
            log.append(
                &Event::ExtensionStateUnset(ExtensionStateUnset {
                    extension: "fiber.test/goal".into(),
                    key: "goal".into(),
                }),
                None,
                None,
            )
            .unwrap();
        }
        drop(log);
        ask(&setup, PARENT);
        assert_eq!(
            seen(&setup, PARENT),
            if removed {
                json!("absent")
            } else {
                json!({"n":3})
            }
        );
    }
}

#[test]
fn resumed_fork_uses_the_parent_value_at_its_point() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::text_reply("done")]).unwrap();
    setup.provider(&server);
    setup.lua("goal", READ);
    let parent = initial(&setup, PARENT, None);
    let point = set(&parent, "goal", json!("at point"));
    set(&parent, "goal", json!("later"));
    drop(parent);
    drop(initial(
        &setup,
        CHILD,
        Some(Point {
            session_id: SessionId(PARENT.into()),
            seq: point,
        }),
    ));
    ask(&setup, CHILD);
    assert_eq!(seen(&setup, CHILD), "at point");
}

#[test]
fn a_loaded_extension_without_hooks_still_drains_init_state() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::text_reply("done")]).unwrap();
    setup.provider(&server);
    setup.lua("goal", "state.set('boot',1)");
    drop(initial(&setup, PARENT, None));
    ask(&setup, PARENT);
    let lines = log::read(&support::session_dir(&setup, PARENT)).unwrap();
    let boot = lines
        .iter()
        .position(|line| line.kind == "extension_state_set" && line.payload["key"] == "boot")
        .unwrap();
    assert_eq!(lines[boot].payload["value"], 1);
    assert_eq!(lines.last().unwrap().kind, "fiber_exited");
    assert!(boot < lines.len() - 1);
    assert!(
        boot < lines
            .iter()
            .position(|line| line.kind == "turn_started")
            .unwrap()
    );
}

#[test]
fn resumed_maximum_depth_state_reads_back_through_the_log_reader() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::text_reply("done")]).unwrap();
    setup.provider(&server);
    setup.lua("goal", READ);
    let mut value = json!([]);
    for _ in 1..125 {
        value = Value::Array(vec![value]);
    }
    let log = initial(&setup, PARENT, None);
    set(&log, "goal", value.clone());
    drop(log);
    ask(&setup, PARENT);
    assert_eq!(seen(&setup, PARENT), value);
}

#[test]
fn a_handoff_keeps_the_live_state_for_its_session_start() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([
        support::text_reply("first"),
        support::text_reply("after handoff"),
    ])
    .unwrap();
    setup.provider(&server);
    setup.lua("goal","state.set('goal',7);fiber.hook('before_handoff',{timeout=5000,on_failure='blocking',run=function()state.set('goal',9);return {note='keep working'} end});fiber.hook('session_start',{timeout=5000,on_failure='blocking',run=function(arg) if arg.reason=='handoff' then state.set('seen',state.get('goal')) end end})");
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    extension_harness::send(
        &client,
        r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    extension_harness::send(
        &client,
        r#"{"id":"prompt","command":"prompt","args":{"content":[{"type":"text","text":"hello"}]}}"#,
    );
    extension_harness::until_session(&client, "first turn completed", |line| {
        line["kind"] == "turn_completed"
    });
    extension_harness::send(&client, r#"{"id":"handoff","command":"handoff","args":{}}"#);
    extension_harness::until_session(&client, "handoff completed", |line| {
        line["kind"] == "handoff_completed"
    });
    extension_harness::until_session(&client, "handoff turn completed", |line| {
        line["kind"] == "turn_completed"
    });
    extension_harness::send(&client, r#"{"id":"close","command":"close","args":{}}"#);
    extension_harness::until_session_close(&client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "{stderr}");
    assert_eq!(seen(&setup, &id), 9);
}

fn hub_command(id: &str, session: &str, command: &str, args: Value) -> String {
    json!({"id":id,"session_id":session,"command":command,"args":args}).to_string()
}

#[test]
fn live_rewind_and_its_resume_use_the_same_parent_point_seed() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        support::text_reply("parent"),
        support::text_reply("child"),
        support::text_reply("resumed"),
    ])
    .unwrap();
    setup.provider(&server);
    setup.lua("goal",&format!("local sid;fiber.hook('session_start',{{timeout=5000,on_failure='blocking',run=function(arg)sid=arg.session_id;state.set('seen',state.get('goal'))end}});fiber.hook('before_model_call',{{timeout=5000,on_failure='blocking',run=function()if sid=='{PARENT}' then state.set('goal','later')end end}})"));
    let parent = initial(&setup, PARENT, None);
    set(&parent, "goal", json!("at point"));
    drop(parent);
    ask(&setup, PARENT);
    let parent_lines = log::read(&support::session_dir(&setup, PARENT)).unwrap();
    let point = parent_lines
        .iter()
        .find(|line| line.kind == "step_started")
        .unwrap()
        .seq
        .unwrap()
        .0
        - 1;
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = support::connect_hub(&setup, &hub);
    let guard =
        support::SessionGuard::arm(setup.deadline, &setup.workspace().display().to_string());
    client.send(&hub_command(
        "sub",
        PARENT,
        "subscribe",
        json!({"level":"full"}),
    ));
    support::until(&client, "parent subscribed before rewind", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "sub"
    });
    client.send(&hub_command(
        "rewind",
        PARENT,
        "rewind",
        json!({"seq":point}),
    ));
    let ack = support::until(&client, "rewind acknowledged", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "rewind"
    });
    let child = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    support::until(&client, "child extensions loaded", |line| {
        line["session_id"] == child && line["kind"] == "extensions_loaded"
    });
    client.send(&hub_command(
        "prompt",
        &child,
        "prompt",
        json!({"content":[{"type":"text","text":"child"}]}),
    ));
    support::until(&client, "child turn completed", |line| {
        line["session_id"] == child && line["kind"] == "turn_completed"
    });
    client.send(&hub_command("close", &child, "close", json!({})));
    support::until(&client, "child exited", |line| {
        line["session_id"] == child && line["kind"] == "fiber_exited"
    });
    drop(client);
    guard.wait_gone();
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    assert_eq!(seen(&setup, &child), "at point");
    ask(&setup, &child);
    assert_eq!(seen(&setup, &child), "at point");
}
