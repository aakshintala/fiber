//! `take_parked` (`docs/extensions.md`, "Host calls"): a failed drive
//! spawn drops exactly the callback `settle` just parked, and nothing else.
//! `settle` starts no host work for a stopped extension, and a cancel that
//! lands after a callback returned leaves its result.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::time::Duration;

use super::*;

/// One parked callback with `id`: the thread is a real Lua coroutine, so
/// the lookup sees the same entries `settle` holds.
fn entry(lua: &mlua::Lua, id: u64) -> Parked {
    let run = lua
        .create_function(|_, ()| Ok(()))
        .expect("a test function builds");
    let thread = lua.create_thread(run).expect("a test thread builds");
    Parked {
        id,
        thread,
        target: Target::Command("go".to_owned()),
        deadline: None,
        timeout: Duration::from_millis(100),
        wake: None,
        _cancel: None,
        ask: None,
        exec: false,
    }
}

fn ids(parked: &[Parked]) -> Vec<u64> {
    parked.iter().map(|p| p.id).collect()
}

/// The matching parked callback goes; the rest keep their places.
#[test]
fn take_parked_removes_only_the_matching_call() {
    let lua = mlua::Lua::new();
    let mut parked = vec![entry(&lua, 1), entry(&lua, 2), entry(&lua, 3)];
    assert!(take_parked(&mut parked, 2), "the parked call is dropped");
    assert_eq!(ids(&parked), [1, 3]);
}

/// Nothing matches: nothing goes, so a flipped comparison dropping the
/// first entry instead would fail here.
#[test]
fn take_parked_leaves_everything_when_nothing_matches() {
    let lua = mlua::Lua::new();
    let mut parked = vec![entry(&lua, 1), entry(&lua, 3)];
    assert!(!take_parked(&mut parked, 2), "no other call is dropped");
    assert_eq!(ids(&parked), [1, 3]);
}

/// A thread `Start` for `dir`, outside any session.
fn start_in(dir: &std::path::Path) -> Start {
    Start {
        name: "fiber.test/stopped".to_owned(),
        dir: dir.to_path_buf(),
        home: dir.to_path_buf(),
        load_by: None,
        memory_cap: super::super::MEMORY_CAP,
        browser: Arc::new(crate::oauth::SystemBrowser::default()),
        session: None,
        secrets: Vec::new(),
    }
}

/// Once the extension is stopped, a callback that suspends on `host.exec`
/// starts no run: the scripted run stays unused, nothing is parked and no
/// `extension_exec` is routed.
#[test]
fn a_stopped_extension_starts_no_exec_for_a_suspended_callback() {
    let dir = fakes::TempDir::new("fiber-schedule-stopped");
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let script = crate::host::script::HostScript::new(
        Vec::new(),
        vec![crate::host::script::ExecEntry {
            request: serde_json::json!({}),
            reply: Ok(crate::host::script::ExecReply {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }),
        }],
    );
    hub.set_host_script(Arc::clone(&script));
    let id = {
        let mut shared = hub.lock();
        let id = shared.push(
            Target::Tool("t".to_owned()),
            serde_json::Value::Null,
            hub.clock().now(),
        );
        shared.phase = Phase::Stopped(Error::Stopped {
            extension: "fiber.test/stopped".to_owned(),
        });
        id
    };
    let lua = mlua::Lua::new();
    let thread = entry(&lua, id).thread;
    let mut parked = Vec::new();
    let step = Ok(Step::Suspend {
        thread,
        deadline: None,
        timeout: Duration::from_millis(100),
        target: Target::Tool("t".to_owned()),
        request: Request::Exec(exec::ExecRequest {
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), "exit 0".to_owned()],
            cwd: dir.path().to_path_buf(),
            cap: 1024,
        }),
    });
    settle(
        &start_in(dir.path()),
        &hub,
        &mut parked,
        id,
        &Target::Tool("t".to_owned()),
        step,
    );
    assert!(parked.is_empty(), "nothing is parked");
    assert_eq!(script.unmet().len(), 1, "the scripted run is unused");
    assert!(hub.lock().buffer.is_empty(), "no extension_exec is routed");
}

/// The caller's result for the tool call `id`.
fn judged(shared: &mut Shared, id: u64, now: Instant) -> Option<serde_json::Value> {
    match shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now) {
        (Some(super::super::Next::Return(Ok(value))), _) => Some(value),
        (Some(_), _) => panic!("the call's own result"),
        (None, _) => None,
    }
}

/// A callback that returned keeps what it returned, whether the cancel
/// landed after it returned or while it ran.
#[test]
fn a_cancel_that_meets_a_returned_callback_leaves_its_result() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let done = serde_json::json!("done");
    let after = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.finish(after, Ok(done.clone()));
    shared.cancel_call(after);
    assert_eq!(judged(&mut shared, after, now), Some(done.clone()));
    let during = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        during,
        Progress::Started {
            deadline: None,
            parked: false,
        },
    );
    shared.cancel_call(during);
    shared.finish(during, Ok(done.clone()));
    assert_eq!(judged(&mut shared, during, now), Some(done));
}
