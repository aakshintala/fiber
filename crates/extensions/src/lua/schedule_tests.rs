//! `take_parked` (`docs/extensions.md`, "Host calls"): a failed drive
//! spawn drops exactly the callback `settle` just parked, and nothing else.
//! `settle` starts no host work for a stopped extension, and a cancel that
//! lands after a callback returned leaves its result. Exec admission
//! (`docs/tools.md`, "Cancellation"): a run is admitted only while ready,
//! its stop reaches the run without the Lua thread, and a cancelled call
//! waits for its group to empty, even abandoned.

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

/// A hub whose phase is ready: admitted work starts.
fn ready_hub() -> Arc<Hub> {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    hub.lock().phase = Phase::Ready(Default::default());
    hub
}

/// An exec run is admitted only while the extension is ready: stopping
/// first admits nothing and registers nothing, so a flipped phase check
/// admitting while stopped would fail here.
#[test]
fn admit_exec_registers_only_while_ready() {
    let clock = fakes::clock::FakeClock::new();
    let hub = Hub::new(clock.clone());
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    assert!(shared.admit_exec(id).is_none(), "idle admits no run");
    assert!(!shared.exec_pending(id), "nothing is registered");
    shared.phase = Phase::Stopped(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    assert!(shared.admit_exec(id).is_none(), "stopped admits no run");
    assert!(!shared.exec_pending(id), "nothing is registered");
    shared.phase = Phase::Ready(Default::default());
    let rx = shared.admit_exec(id);
    assert!(rx.is_some(), "ready admits the run");
    assert!(shared.exec_pending(id), "the run is registered");
}

/// A cancel signal delivered before exec admission leaves no run to spawn:
/// admitting despite the cancelled mark would fail the first assertion.
#[test]
fn cancel_before_exec_admission_starts_no_run() {
    let hub = ready_hub();
    let now = hub.clock().now();
    let id = {
        let mut shared = hub.lock();
        let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
        shared.calls.insert(
            id,
            Progress::Started {
                deadline: None,
                parked: false,
            },
        );
        id
    };
    let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
    let cancelling_hub = Arc::clone(&hub);
    let cancelling = std::thread::spawn(move || {
        cancelling_hub.lock().cancel_call(id);
        cancelled_tx.send(()).expect("cancellation is signalled");
    });
    cancelled_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("cancellation is ordered before admission");
    cancelling.join().expect("the cancellation thread ends");

    let mut shared = hub.lock();
    assert!(
        shared.admit_exec(id).is_none(),
        "cancelled work is not admitted"
    );
    assert!(
        !shared.exec_pending(id),
        "no stop sender or run is registered"
    );
}

/// Host work is admitted only while the extension is ready: the drive and
/// HTTP workers share this check, so a flipped check admitting while
/// stopped would fail here.
#[test]
fn admit_work_holds_only_while_ready() {
    let clock = fakes::clock::FakeClock::new();
    let hub = Hub::new(clock);
    let mut shared = hub.lock();
    assert!(!shared.admit_work(), "idle admits no work");
    shared.phase = Phase::Stopped(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    assert!(!shared.admit_work(), "stopped admits no work");
    shared.phase = Phase::Ready(Default::default());
    assert!(shared.admit_work(), "ready admits work");
}

/// Stopping the extension stops its admitted exec without the Lua thread:
/// the stop sender drops, so the run's receiver disconnects, while the
/// entry stays registered for the caller to await. A `stop` that left the
/// sender would fail here.
#[test]
fn stopping_an_extension_stops_its_admitted_exec() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    let rx = shared.admit_exec(id).expect("ready admits the run");
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "no stop is signalled yet"
    );
    shared.stop(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    drop(shared);
    // The sender dropped with the stop: the receiver disconnects.
    assert!(
        matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the stop reaches the run without the Lua thread"
    );
    assert!(hub.lock().exec_pending(id), "the run stays to be awaited");
}

/// Cancelling a started call stops its admitted exec but keeps the entry:
/// a `cancel_call` that left the sender would fail here, as would one
/// that dropped the entry the caller awaits.
#[test]
fn cancelling_a_started_call_stops_its_admitted_exec() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            parked: true,
        },
    );
    let rx = shared.admit_exec(id).expect("ready admits the run");
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "no stop is signalled yet"
    );
    shared.cancel_call(id);
    drop(shared);
    assert!(
        matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "the cancel reaches the run without the Lua thread"
    );
    assert!(hub.lock().exec_pending(id), "the run stays to be awaited");
}

/// Cancelling a call with no admitted exec is a no-op for the registry:
/// the missing entry stays missing, so a flipped lookup inserting one
/// would fail here.
#[test]
fn cancelling_without_an_admitted_exec_registers_nothing() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            parked: true,
        },
    );
    shared.cancel_call(id);
    assert!(!shared.exec_pending(id), "nothing is registered");
}

/// Whether the tool call `id` still waits for its exec run.
fn waits(shared: &mut Shared, id: u64, now: Instant) -> bool {
    matches!(
        shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now),
        (Some(super::super::Next::Sleep(_)), _)
    )
}

/// A cancelled call abandoned with its exec run waits for the group to
/// empty instead of returning: it sleeps while the run is admitted, and
/// ends cancelled once the run's end clears it. Returning `None` while
/// admitted would fail here.
#[test]
fn a_cancelled_abandoned_call_waits_for_its_exec() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            parked: true,
        },
    );
    shared.admit_exec(id).expect("ready admits the run");
    shared.cancel_call(id);
    shared.phase = Phase::Stopped(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    assert!(waits(&mut shared, id, now), "the call waits for its run");
    shared.finish_exec(id);
    let (judged, _) =
        shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now);
    assert!(judged.is_none(), "the call ends cancelled once emptied");
}

/// A cancelled call abandoned with no admitted run ends at once: nothing
/// waits, so waiting instead would fail here.
#[test]
fn a_cancelled_abandoned_call_without_exec_ends_at_once() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            parked: true,
        },
    );
    shared.cancel_call(id);
    shared.phase = Phase::Stopped(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    let (judged, _) =
        shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now);
    assert!(judged.is_none(), "the call ends cancelled at once");
}

/// An uncancelled call abandoned with an admitted run still fails: only a
/// cancelled call waits for the group, so waiting here would fail.
#[test]
fn an_uncancelled_abandoned_call_with_exec_still_fails() {
    let hub = ready_hub();
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            parked: true,
        },
    );
    shared.admit_exec(id).expect("ready admits the run");
    shared.phase = Phase::Stopped(Error::Stopped {
        extension: "fiber.test/stopped".to_owned(),
    });
    match shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now) {
        (Some(super::super::Next::Return(Err(_))), _) => {}
        _ => panic!("the call fails"),
    }
}

/// Clears the settle hook when it drops, so a failing test leaves no hook
/// for the next one.
struct SettleGuard;

impl Drop for SettleGuard {
    fn drop(&mut self) {
        unpause_settle();
    }
}

/// An abandon between the phase check and the spawn starts no host work:
/// the admission rechecks see Stopped, so nothing parks, nothing registers
/// and neither marker stays absent by luck. Each settle runs on its own
/// ready hub, and one hook abandons whichever hub the calling tool belongs
/// to, ordered by the hook call itself, with no sleeps. Without the
/// rechecks the spawns would park the callbacks, failing the first asserts.
#[test]
fn abandon_between_check_and_spawn_starts_no_host_work() {
    let dir = fakes::TempDir::new("fiber-schedule-race");
    let exec_marker = dir.path().join("exec-started");
    let drive_marker = dir.path().join("drive-started");
    let exec_hub = ready_hub();
    let drive_hub = ready_hub();
    drive_hub.set_driver(Arc::new(TouchDrive {
        marker: drive_marker.clone(),
    }) as Arc<dyn contract::extension::Drive>);
    let exec_id = {
        let mut shared = exec_hub.lock();
        shared.push(
            Target::Tool("race".to_owned()),
            serde_json::Value::Null,
            exec_hub.clock().now(),
        )
    };
    let drive_id = {
        let mut shared = drive_hub.lock();
        shared.push(
            Target::Tool("race-drive".to_owned()),
            serde_json::Value::Null,
            drive_hub.clock().now(),
        )
    };
    pause_settle({
        let exec_hub = Arc::clone(&exec_hub);
        let drive_hub = Arc::clone(&drive_hub);
        Arc::new(move |target: &Target| {
            let stopped = || Error::Stopped {
                extension: "fiber.test/stopped".to_owned(),
            };
            let hub = match target {
                Target::Tool(name) if name == "race" => Some(Arc::clone(&exec_hub)),
                Target::Tool(name) if name == "race-drive" => Some(Arc::clone(&drive_hub)),
                Target::Command(_)
                | Target::Provider { .. }
                | Target::Hook { .. }
                | Target::Timer { .. }
                | Target::Tool(_)
                | Target::Effects(_) => None,
            };
            if let Some(hub) = hub {
                let unsent = {
                    let mut shared = hub.lock();
                    shared.stop(stopped())
                };
                drop(unsent);
            }
        })
    });
    let _guard = SettleGuard;
    let lua = mlua::Lua::new();
    let thread = entry(&lua, exec_id).thread;
    let mut parked = Vec::new();
    settle(
        &start_in(dir.path()),
        &exec_hub,
        &mut parked,
        exec_id,
        &Target::Tool("race".to_owned()),
        Ok(Step::Suspend {
            thread,
            deadline: None,
            timeout: Duration::from_millis(100),
            target: Target::Tool("race".to_owned()),
            request: Request::Exec(exec::ExecRequest {
                program: "sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    format!("touch '{}'", exec_marker.display()),
                ],
                cwd: dir.path().to_path_buf(),
                cap: 1024,
            }),
        }),
    );
    assert!(parked.is_empty(), "nothing is parked");
    assert!(
        !exec_hub.lock().exec_pending(exec_id),
        "nothing is admitted"
    );
    assert!(!exec_marker.exists(), "no exec starts after the abandon");
    let thread = entry(&lua, drive_id).thread;
    let mut parked = Vec::new();
    settle(
        &start_in(dir.path()),
        &drive_hub,
        &mut parked,
        drive_id,
        &Target::Tool("race-drive".to_owned()),
        Ok(Step::Suspend {
            thread,
            deadline: None,
            timeout: Duration::from_millis(100),
            target: Target::Tool("race-drive".to_owned()),
            request: Request::Drive(crate::host::DriveRequest {
                command: "prompt".to_owned(),
                args: serde_json::Map::new(),
            }),
        }),
    );
    assert!(parked.is_empty(), "nothing is parked");
    assert!(!drive_marker.exists(), "no drive starts after the abandon");
}

/// A driver that marks `marker` when a drive starts: reaching it means the
/// race was lost.
struct TouchDrive {
    marker: std::path::PathBuf,
}

impl contract::extension::Drive for TouchDrive {
    fn drive(
        &self,
        _extension: &str,
        _command: &str,
        _args: serde_json::Map<String, serde_json::Value>,
        _answer: contract::inbox::Ack,
    ) {
        match std::fs::write(&self.marker, "ran") {
            Ok(()) | Err(_) => {}
        }
    }
}
