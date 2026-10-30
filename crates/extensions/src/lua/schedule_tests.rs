//! `take_parked` (`docs/extensions.md`, "Host calls"): a failed drive
//! spawn drops exactly the callback `settle` just parked, and nothing else.
//! `settle` starts no host work for a stopped extension, and a cancel that
//! lands after a callback returned leaves its result. Admission
//! (`docs/tools.md`, "Cancellation"): host work starts only while ready and
//! never for a cancelled call, which ends there; an exec run's stop reaches
//! it without the Lua thread, and a cancelled call waits for its group to
//! empty, even abandoned.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::BTreeMap;
use std::time::Duration;

use super::super::CredentialFor;
use super::super::host_calls::{Pause, pause_settle, unpause_settle};
use super::*;
use crate::host::{Request, exec};
use fakes::Deadline;

const STATE_INIT: &str = "fiber.hook('before_message',{timeout=5000,on_failure='blocking',run=function() state.set('k',1) end})";

#[test]
fn state_hook_trial_opens_at_push_and_never_reopens_at_a_late_start() {
    use crate::host::state::{Scope, StateRegistry, VmState};
    use contract::events::OnFork;
    let registry = StateRegistry::standalone();
    let store = registry.store("goal");
    let clock = fakes::clock::FakeClock::new();
    let hub = Hub::new(clock.clone());
    hub.lock().state = store.clone();
    let vm = VmState {
        store: store.clone(),
        scope: std::cell::Cell::new(Scope::Other),
    };
    let target = || Target::Hook {
        point: "before_message".into(),
        index: 0,
    };
    let id = hub
        .lock()
        .push(target(), serde_json::Value::Null, hub.clock().now());
    vm.enter(Scope::Hook(id));
    vm.set("k", serde_json::json!(1), OnFork::AtPoint).unwrap();
    assert_eq!(
        store.take_call(id).len(),
        1,
        "push opened the trial without a worker"
    );
    assert!(registry.take_outbox().is_empty());
    let command = hub.lock().push(
        Target::Command("test".into()),
        serde_json::Value::Null,
        hub.clock().now(),
    );
    vm.enter(Scope::Hook(command));
    vm.set("command", serde_json::json!(true), OnFork::AtPoint)
        .unwrap();
    assert_eq!(store.get("command"), None);
    assert!(store.take_call(command).is_empty());
    let discarded = hub
        .lock()
        .push(target(), serde_json::Value::Null, hub.clock().now());
    store.discard_call(discarded);
    vm.enter(Scope::Hook(discarded));
    vm.set("discarded", serde_json::json!(true), OnFork::AtPoint)
        .unwrap();
    assert_eq!(store.get("discarded"), None);
    assert!(registry.take_outbox().is_empty());
    drop(hub);

    let registry = StateRegistry::standalone();
    let store = registry.store("goal");
    let hub = Hub::new(clock);
    let dir = fakes::TempDir::new("fiber-state-late-start");
    std::fs::write(dir.path().join("init.lua"), STATE_INIT).unwrap();
    hub.lock().state = store.clone();
    hub.lock().phase = Phase::Registering { abandon_at: None };
    let id = hub
        .lock()
        .push(target(), serde_json::Value::Null, hub.clock().now());
    store.discard_call(id);
    let (done, ended) = std::sync::mpsc::channel();
    let (thread_hub, start) = (hub.clone(), start_in(dir.path()));
    std::thread::spawn(move || {
        serve(thread_hub, start);
        let _sent = done.send(());
    });
    wait_returned(&hub, id);
    assert!(
        matches!(
            hub.lock().calls.get(&id),
            Some(super::super::hub::Progress::Done(Ok(_)))
        ),
        "the discarded call still ran successfully"
    );
    assert_eq!(store.get("k"), None);
    assert!(store.take_call(id).is_empty());
    assert!(registry.take_outbox().is_empty());
    let next = hub
        .lock()
        .push(target(), serde_json::Value::Null, hub.clock().now());
    hub.notify();
    wait_returned(&hub, next);
    assert!(matches!(
        hub.lock().calls.get(&next),
        Some(super::super::hub::Progress::Done(Ok(_)))
    ));
    let lines = store.take_call(next);
    assert_eq!(
        lines.len(),
        1,
        "discarding one call does not close the next"
    );
    assert_eq!(store.get("k"), Some(serde_json::json!(1)));
    assert!(registry.take_outbox().is_empty());
    assert_eq!(registry.admit(lines).len(), 1);
    hub.dispose("goal");
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv_or_fail(&ended, "state test VM stopped");
}

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

/// A ready hub with one 50 ms hook whose running call was judged at its
/// deadline: the caller took `Timeout` and the abandon watch is armed for
/// that call. Returns the hub, the watched id, its target, its timeout and
/// the deadline the watch runs from.
fn armed_hook() -> (Arc<Hub>, u64, Target, Duration, Instant) {
    use super::super::declared::{DeclaredHook, HookPhase};

    let hub = Hub::new(fakes::clock::FakeClock::new());
    let point = "before_message".to_owned();
    let target = Target::Hook {
        point: point.clone(),
        index: 0,
    };
    let timeout = Duration::from_millis(50);
    let now = hub.clock().now();
    let asked = now.checked_sub(timeout).unwrap();
    let deadline = asked.checked_add(timeout).unwrap();
    let watched = {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        if let Phase::Ready(timeouts) = &mut shared.phase {
            timeouts.hooks.by_point.insert(
                point,
                vec![DeclaredHook {
                    phase: HookPhase::Transform,
                    blocking: false,
                    timeout,
                    entry: None,
                    phase_explicit: false,
                }],
            );
        } else {
            panic!("not ready");
        }
        let watched = shared.push(target.clone(), serde_json::Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            watched,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        match shared.judge("ext", watched, &target, asked, now) {
            (super::super::hub::Next::Return(Err(Error::Timeout { .. })), _) => {}
            _ => panic!("the running hook times out at its deadline"),
        }
        assert!(
            shared
                .abandon
                .as_ref()
                .is_some_and(|watch| watch.id == watched),
            "the Timeout arms the abandon watch"
        );
        watched
    };
    (hub, watched, target, timeout, deadline)
}

/// `park` disarms the abandon watch only for the watched hook: parking any
/// other call keeps the watch armed, so the abandonment still fires past
/// the grace; parking the watched hook itself disarms it, so the next judge
/// past the grace finds the VM still up. With the comparison flipped
/// (`!=`) the unrelated park would disarm and the watched park would keep
/// the watch, failing here.
#[test]
fn parking_only_the_watched_hook_disarms_its_abandon_watch() {
    use super::super::hub::Next;

    let past_grace = |deadline: Instant| {
        deadline
            .checked_add(GRACE)
            .and_then(|at| at.checked_add(Duration::from_millis(1)))
            .unwrap()
    };
    // An unrelated park keeps the watch: the next judge past the grace
    // still abandons the VM.
    {
        let lua = mlua::Lua::new();
        let (hub, watched, target, timeout, deadline) = armed_hook();
        let mut shared = hub.lock();
        let mut parked = Vec::new();
        let other = u64::MAX;
        park(
            &mut parked,
            &mut shared,
            other,
            entry(&lua, other).thread,
            Target::Command("other".to_owned()),
            None,
            timeout,
            None,
            None,
            None,
            Run::Parked,
        );
        assert!(
            shared
                .abandon
                .as_ref()
                .is_some_and(|watch| watch.id == watched),
            "parking an unrelated call keeps the watch armed"
        );
        let at = past_grace(deadline);
        let probe = shared.push(target.clone(), serde_json::Value::Null, at);
        match shared.judge("ext", probe, &target, at, at) {
            (Next::Return(Err(Error::Abandoned { .. })), _) => {}
            _ => panic!("the armed watch past its grace abandons the VM"),
        }
        assert!(
            matches!(shared.phase, Phase::Stopped(_)),
            "the VM is abandoned"
        );
    }
    // Parking the watched hook disarms the watch: the next judge past the
    // grace waits for its own deadline with the VM still up.
    {
        let lua = mlua::Lua::new();
        let (hub, watched, target, timeout, deadline) = armed_hook();
        let mut shared = hub.lock();
        let mut parked = Vec::new();
        park(
            &mut parked,
            &mut shared,
            watched,
            entry(&lua, watched).thread,
            target.clone(),
            Some(deadline),
            timeout,
            None,
            None,
            None,
            Run::Parked,
        );
        assert!(
            shared.abandon.is_none(),
            "parking the watched hook disarms the watch"
        );
        let at = past_grace(deadline);
        let probe = shared.push(target.clone(), serde_json::Value::Null, at);
        match shared.judge("ext", probe, &target, at, at) {
            (Next::Sleep(_), _) => {}
            _ => panic!("the disarmed watch abandons nothing"),
        }
        assert!(matches!(shared.phase, Phase::Ready(_)), "the VM stays up");
    }
}

/// A ready hub with one 50 ms command whose running call was judged at its
/// deadline: the caller took `Timeout` and the abandon watch is armed for
/// that call. Returns the hub, the watched id, its target, its timeout and
/// the deadline the watch runs from.
fn armed_command() -> (Arc<Hub>, u64, Target, Duration, Instant) {
    use super::super::declared::DeclaredCommand;

    let hub = Hub::new(fakes::clock::FakeClock::new());
    let target = Target::Command("spin".to_owned());
    let timeout = Duration::from_millis(50);
    let now = hub.clock().now();
    let asked = now.checked_sub(timeout).unwrap();
    let deadline = asked.checked_add(timeout).unwrap();
    let watched = {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        if let Phase::Ready(timeouts) = &mut shared.phase {
            timeouts.commands.insert(
                "spin".to_owned(),
                DeclaredCommand {
                    timeout,
                    description: String::new(),
                },
            );
        } else {
            panic!("not ready");
        }
        let watched = shared.push(target.clone(), serde_json::Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            watched,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        match shared.judge("ext", watched, &target, asked, now) {
            (super::super::hub::Next::Return(Err(Error::Timeout { .. })), _) => {}
            _ => panic!("the running command times out at its deadline"),
        }
        assert!(
            shared
                .abandon
                .as_ref()
                .is_some_and(|watch| watch.id == watched),
            "the Timeout arms the abandon watch"
        );
        watched
    };
    (hub, watched, target, timeout, deadline)
}

/// A watched command's late finish is dropped and disarms its watch, while
/// another id's finish leaves the watch armed: the late result belongs to a
/// caller that already took `Timeout`.
#[test]
fn a_watched_commands_finish_is_dropped_and_disarms_its_watch() {
    // The watched finish disarms the watch and records nothing.
    {
        let (hub, watched, _, _, _) = armed_command();
        hub.finish(watched, Ok(serde_json::json!("late")));
        let shared = hub.lock();
        assert!(
            shared.abandon.is_none(),
            "the watched finish disarms the watch"
        );
        assert!(
            !shared.calls.contains_key(&watched),
            "the late result is dropped"
        );
    }
    // Another id's finish leaves the watch armed.
    {
        let (hub, watched, _, _, _) = armed_command();
        hub.finish(watched.wrapping_add(1), Ok(serde_json::json!("late")));
        let shared = hub.lock();
        assert!(
            shared
                .abandon
                .as_ref()
                .is_some_and(|watch| watch.id == watched),
            "another call's finish keeps the watch armed"
        );
    }
}

/// A command that parks after its caller took `Timeout` ends at its own
/// deadline without abandoning the VM: parking disarmed the watch, the call
/// has no waiter, and the thread fails the parked entry at its deadline.
#[test]
fn a_command_that_parks_after_its_caller_answered_ends_at_its_own_deadline() {
    let lua = mlua::Lua::new();
    let (hub, watched, target, timeout, deadline) = armed_command();
    let mut parked = Vec::new();
    {
        let mut shared = hub.lock();
        park(
            &mut parked,
            &mut shared,
            watched,
            entry(&lua, watched).thread,
            target.clone(),
            Some(deadline),
            timeout,
            None,
            None,
            None,
            Run::Parked,
        );
        assert!(
            shared.abandon.is_none(),
            "parking the watched command disarms the watch"
        );
        assert!(matches!(shared.phase, Phase::Ready(_)), "the VM stays up");
        assert!(
            !shared.calls.contains_key(&watched),
            "the answered call has no waiter"
        );
    }
    // The clock is already at the deadline, so the parked entry fails at
    // its own deadline.
    match next("ext", &hub, &mut parked) {
        Some(Work::Collect) => {}
        _ => panic!("the parked command ends at its own deadline"),
    }
    assert!(
        matches!(hub.lock().phase, Phase::Ready(_)),
        "the VM stays up"
    );
}

/// A thread `Start` for `dir`, outside any session.
fn start_in(dir: &std::path::Path) -> Start {
    Start {
        name: "fiber.test/stopped".to_owned(),
        dir: dir.to_path_buf(),
        home: dir.to_path_buf(),
        run_entry: true,
        load_by: None,
        memory_cap: super::super::MEMORY_CAP,
        browser: Arc::new(crate::oauth::SystemBrowser::default()),
        session: None,
        secrets: Vec::new(),
        deps: Default::default(),
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
            reply: Ok(crate::host::script::ExecReply::Completed {
                end: crate::host::script::ExecEnd::Code(0),
                stdout: String::new(),
                stderr: String::new(),
            }),
        }],
        Vec::new(),
    );
    hub.set_host_script(Arc::clone(&script));
    let id = {
        let mut shared = hub.lock();
        let id = shared.push(
            Target::Tool("t".to_owned()),
            serde_json::Value::Null,
            hub.clock().now(),
        );
        shared.phase = Phase::Stopped(StopReason::Stopped);
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
            env: BTreeMap::new(),
            input: None,
            cap: 1024,
            own_group: true,
            #[cfg(test)]
            test_events: None,
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
    assert!(
        hub.lock().router.buffer().is_empty(),
        "no extension_exec is routed"
    );
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
            run: Run::Running,
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

/// A started tool call `id` on `hub`, as `next` leaves it for `settle`.
fn started(hub: &Hub) -> u64 {
    let mut shared = hub.lock();
    let now = hub.clock().now();
    let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: None,
            run: Run::Running,
        },
    );
    id
}

/// Host work is admitted only while the extension is ready: a flipped
/// phase check admitting while idle or stopped would fail here.
#[test]
fn admit_holds_only_while_ready() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let id = started(&hub);
    assert!(hub.admit(id).is_none(), "idle admits no work");
    hub.lock().phase = Phase::Stopped(StopReason::Stopped);
    assert!(hub.admit(id).is_none(), "stopped admits no work");
    hub.lock().phase = Phase::Ready(Default::default());
    assert!(hub.admit(id).is_some(), "ready admits work");
}

/// A cancelled call admits no host work and ends cancelled there, while an
/// uncancelled one is admitted and left started: admitting despite the
/// mark, or leaving the cancelled call started, would fail here.
#[test]
fn admit_ends_a_cancelled_call_and_admits_the_rest() {
    let hub = ready_hub();
    let kept = started(&hub);
    let cancelled = started(&hub);
    hub.lock().cancel_call(cancelled);
    assert!(
        hub.admit(cancelled).is_none(),
        "cancelled work is not admitted"
    );
    assert!(hub.admit(kept).is_some(), "uncancelled work is admitted");
    let mut shared = hub.lock();
    let now = hub.clock().now();
    assert!(
        matches!(shared.calls.get(&kept), Some(Progress::Started { .. })),
        "the admitted call stays started"
    );
    assert!(
        judged(&mut shared, cancelled, now).is_none(),
        "the cancelled call ends cancelled"
    );
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
    let rx = shared.register_exec(id);
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "no stop is signalled yet"
    );
    shared.stop(StopReason::Stopped);
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
            run: Run::Parked,
        },
    );
    let rx = shared.register_exec(id);
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
            run: Run::Parked,
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
            run: Run::Parked,
        },
    );
    shared.register_exec(id);
    shared.cancel_call(id);
    shared.phase = Phase::Stopped(StopReason::Stopped);
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
            run: Run::Parked,
        },
    );
    shared.cancel_call(id);
    shared.phase = Phase::Stopped(StopReason::Stopped);
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
            run: Run::Parked,
        },
    );
    shared.register_exec(id);
    shared.phase = Phase::Stopped(StopReason::Stopped);
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

/// An abandon before admission starts no host work: `Hub::admit` sees
/// Stopped, so nothing parks, nothing registers and neither marker stays
/// absent by luck. Each settle runs on its own ready hub, and one hook
/// abandons whichever hub the calling tool belongs to, ordered by the hook
/// call itself, with no sleeps. Without the phase check the spawns would
/// park the callbacks, failing the first asserts.
#[test]
fn an_abandon_before_admission_starts_no_host_work() {
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
        Arc::new(move |target: &Target, pause: Pause| {
            if pause != Pause::Admission {
                return;
            }
            let hub = match target {
                Target::Tool(name) if name == "race" => Some(Arc::clone(&exec_hub)),
                Target::Tool(name) if name == "race-drive" => Some(Arc::clone(&drive_hub)),
                Target::Command(_)
                | Target::Provider { .. }
                | Target::Hook { .. }
                | Target::Watcher { .. }
                | Target::Timer { .. }
                | Target::Tool(_)
                | Target::Delegate { .. }
                | Target::Search(_)
                | Target::Effects(_) => None,
            };
            if let Some(hub) = hub {
                let unsent = {
                    let mut shared = hub.lock();
                    shared.stop(StopReason::Stopped)
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
                env: BTreeMap::new(),
                input: None,
                cap: 1024,
                own_group: true,
                #[cfg(test)]
                test_events: None,
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

/// A cancel ordered before admission by the settle hook starts no host
/// work and ends the call: nothing parks, no run registers, neither marker
/// appears, and the waiter finds the call cancelled rather than started.
/// Without the cancelled check in `Hub::admit`, the drive would start and
/// the exec would park; without its ending, the call would stay started.
#[test]
fn a_cancel_before_admission_starts_no_host_work_and_ends_the_call() {
    let dir = fakes::TempDir::new("fiber-schedule-cancel");
    let exec_marker = dir.path().join("exec-started");
    let drive_marker = dir.path().join("drive-started");
    let hub = ready_hub();
    hub.set_driver(Arc::new(TouchDrive {
        marker: drive_marker.clone(),
    }) as Arc<dyn contract::extension::Drive>);
    pause_settle({
        let hub = Arc::clone(&hub);
        Arc::new(move |target: &Target, pause: Pause| {
            if pause != Pause::Admission {
                return;
            }
            let name = match target {
                Target::Tool(name) | Target::Search(name) => Some(name.as_str()),
                Target::Provider { name, .. } => Some(name.as_str()),
                Target::Command(_)
                | Target::Hook { .. }
                | Target::Watcher { .. }
                | Target::Timer { .. }
                | Target::Delegate { .. }
                | Target::Effects(_) => None,
            };
            if let Some(id) = name
                .and_then(|name| name.strip_prefix("cancel-"))
                .and_then(|id| id.parse().ok())
            {
                hub.lock().cancel_call(id);
            }
        })
    });
    let _guard = SettleGuard;
    let lua = mlua::Lua::new();
    // Held for the test: a cancelled call never binds, so the port only
    // names it, and holding it leaves no gap for another listener.
    let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("a held loopback port");
    let callback_port = held.local_addr().expect("a bound port").port();
    for case in ["exec", "drive", "callback", "lock"] {
        let id = started(&hub);
        let pair = crate::lua_provider::CredentialPair {
            credential: "acme".to_owned(),
            label: "default".to_owned(),
        };
        let (target, request) = match case {
            "exec" => (
                Target::Tool(format!("cancel-{id}")),
                Request::Exec(exec::ExecRequest {
                    program: "sh".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        format!("touch '{}'", exec_marker.display()),
                    ],
                    cwd: dir.path().to_path_buf(),
                    env: BTreeMap::new(),
                    input: None,
                    cap: 1024,
                    own_group: true,
                    #[cfg(test)]
                    test_events: None,
                }),
            ),
            "drive" => (
                Target::Tool(format!("cancel-{id}")),
                Request::Drive(crate::host::DriveRequest {
                    command: "prompt".to_owned(),
                    args: serde_json::Map::new(),
                }),
            ),
            "callback" => (
                Target::Tool(format!("cancel-{id}")),
                Request::Callback {
                    port: callback_port,
                    path: None,
                },
            ),
            _ => (
                Target::Provider {
                    name: format!("cancel-{id}"),
                    function: "credential",
                    credential: Some(CredentialFor::Stored(pair)),
                },
                Request::Lock,
            ),
        };
        let mut parked = Vec::new();
        settle(
            &start_in(dir.path()),
            &hub,
            &mut parked,
            id,
            &target,
            Ok(Step::Suspend {
                thread: entry(&lua, id).thread,
                deadline: None,
                timeout: Duration::from_millis(100),
                target: target.clone(),
                request,
            }),
        );
        assert!(parked.is_empty(), "nothing is parked");
        let shared = hub.lock();
        assert!(!shared.exec_pending(id), "no run is registered");
        assert!(
            matches!(shared.calls.get(&id), Some(Progress::Cancelled)),
            "the call ended cancelled"
        );
    }
    assert!(!exec_marker.exists(), "no exec starts for a cancelled call");
    assert!(
        !drive_marker.exists(),
        "no drive starts for a cancelled call"
    );
}

/// Neither `oauth::listen` nor `oauth::lock` starts once its call has been
/// cancelled or its extension has stopped: both start under the admission
/// lock, so no cancel or stop lands between admission and start. Each case
/// runs `settle` on a worker with the must-succeed bound; the `Pause::Start` hook
/// ends the call only when the start runs outside the lock (`Hub::try_lock`
/// succeeds). Without the lock hold the end lands, the start still goes
/// ahead and the parked entry keeps the port bound, failing the first
/// asserts below.
#[test]
fn oauth_starts_under_the_admission_lock_so_no_cancel_or_stop_precedes_it() {
    for (request_kind, ending) in [
        ("callback", "cancel"),
        ("callback", "stop"),
        ("lock", "cancel"),
        ("lock", "stop"),
    ] {
        let case = format!("{request_kind}/{ending}");
        let dir = fakes::TempDir::new("fiber-schedule-oauth-race");
        let hub = ready_hub();
        let id = started(&hub);
        let name = format!("race-oauth-{id}");
        /// A browser holding the callback listener the start hands over
        /// through `callback_listener`, so choosing and binding the port
        /// leave no gap for another listener.
        struct Handover {
            listener: std::sync::Mutex<Option<std::net::TcpListener>>,
        }
        impl crate::oauth::Browser for Handover {
            fn open(&self, _url: &str) {}
            fn show(&self, _url: &str, _code: &str) {}
            fn attended(&self) -> bool {
                true
            }
            fn callback_listener(&self, port: u16) -> std::io::Result<std::net::TcpListener> {
                if let Some(listener) = self.listener.lock().unwrap().take() {
                    return Ok(listener);
                }
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            }
        }
        let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a held loopback port");
        let port = held.local_addr().expect("a bound port").port();
        let browser: Arc<dyn crate::oauth::Browser> = Arc::new(Handover {
            listener: std::sync::Mutex::new(Some(held)),
        });
        let target = Target::Provider {
            name: name.clone(),
            function: "credential",
            credential: Some(CredentialFor::Stored(crate::CredentialPair {
                credential: "acme".to_owned(),
                label: "default".to_owned(),
            })),
        };
        let request = if request_kind == "callback" {
            Request::Callback { port, path: None }
        } else {
            Request::Lock
        };
        let landed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        pause_settle({
            let hub = Arc::clone(&hub);
            let landed = Arc::clone(&landed);
            let held = Arc::clone(&held);
            Arc::new(move |target: &Target, pause: Pause| {
                if pause != Pause::Start {
                    return;
                }
                if !matches!(target, Target::Provider { name: wanted, .. } if wanted == &name) {
                    return;
                }
                match hub.try_lock() {
                    Some(mut guard) => {
                        // The start runs outside the admission lock: end the
                        // call the way a racing cancel or stop would.
                        landed.store(true, std::sync::atomic::Ordering::SeqCst);
                        if ending == "cancel" {
                            guard.cancel_call(id);
                        } else {
                            let unsent = guard.stop(StopReason::Stopped);
                            drop(guard);
                            drop(unsent);
                        }
                    }
                    None => {
                        held.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            })
        });
        let _guard = SettleGuard;
        let (settled_tx, settled_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        {
            let hub = Arc::clone(&hub);
            let target = target.clone();
            let home = dir.path().to_path_buf();
            let browser = Arc::clone(&browser);
            std::thread::spawn(move || {
                let lua = mlua::Lua::new();
                let thread = entry(&lua, id).thread;
                let mut parked = Vec::new();
                settle(
                    &Start {
                        name: "fiber.test/stopped".to_owned(),
                        dir: home.clone(),
                        home,
                        run_entry: true,
                        load_by: None,
                        memory_cap: super::super::MEMORY_CAP,
                        browser,
                        session: None,
                        secrets: Vec::new(),
                        deps: Default::default(),
                    },
                    &hub,
                    &mut parked,
                    id,
                    &target,
                    Ok(Step::Suspend {
                        thread,
                        deadline: None,
                        timeout: Duration::from_millis(100),
                        target: target.clone(),
                        request,
                    }),
                );
                let summary: Vec<(u64, bool)> =
                    parked.iter().map(|p| (p.id, p._cancel.is_some())).collect();
                match settled_tx.send(summary) {
                    Ok(()) | Err(_) => {}
                }
                // Held until the asserts ran: dropping it frees the port
                // and ends the wait.
                match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&done_rx) {
                    Ok(()) | Err(_) => {}
                }
            });
        }
        let summary: Vec<(u64, bool)> = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&settled_rx)
            .unwrap_or_else(|_| {
                panic!("settle returned for {case}: calling code that blocks is a wait too")
            });
        let ended = {
            let shared = hub.lock();
            shared.cancelled.contains(&id) || !matches!(shared.phase, Phase::Ready(_))
        };
        if ended {
            assert!(
                summary.is_empty(),
                "no lock wait began for an ended call in {case}"
            );
            if request_kind == "callback" {
                assert!(
                    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok(),
                    "no port was bound for an ended call in {case}"
                );
            }
        }
        assert!(
            held.load(std::sync::atomic::Ordering::SeqCst),
            "the start ran under the admission lock in {case}"
        );
        assert!(
            !landed.load(std::sync::atomic::Ordering::SeqCst),
            "no cancel or stop preceded the start in {case}"
        );
        assert_eq!(
            summary,
            vec![(id, true)],
            "the start parked its cancel handle in {case}"
        );
        match done_tx.send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// A failed OAuth start delivers its failure after releasing the lock: a
/// taken port and the reserved `default.lock` label both fail before any
/// wait starts, and `settle` returns instead of deadlocking on its own
/// delivery. A delivery under the lock would time out here.
#[test]
fn a_failed_oauth_start_delivers_its_failure_after_releasing_the_lock() {
    let dir = fakes::TempDir::new("fiber-schedule-oauth-failure");
    let taken = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("a held loopback port");
    let port = taken.local_addr().expect("a bound port").port();
    for request_kind in ["callback", "lock"] {
        let hub = ready_hub();
        let id = started(&hub);
        let (target, request) = if request_kind == "callback" {
            (
                Target::Tool("fail-oauth".to_owned()),
                Request::Callback { port, path: None },
            )
        } else {
            (
                Target::Provider {
                    name: "fail-oauth".to_owned(),
                    function: "credential",
                    credential: Some(CredentialFor::Stored(crate::CredentialPair {
                        credential: "acme".to_owned(),
                        label: "default.lock".to_owned(),
                    })),
                },
                Request::Lock,
            )
        };
        let (settled_tx, settled_rx) = std::sync::mpsc::channel();
        {
            let hub = Arc::clone(&hub);
            let target = target.clone();
            let home = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let lua = mlua::Lua::new();
                let thread = entry(&lua, id).thread;
                let mut parked = Vec::new();
                settle(
                    &start_in(&home),
                    &hub,
                    &mut parked,
                    id,
                    &target,
                    Ok(Step::Suspend {
                        thread,
                        deadline: None,
                        timeout: Duration::from_millis(100),
                        target: target.clone(),
                        request,
                    }),
                );
                let summary: Vec<(u64, bool)> =
                    parked.iter().map(|p| (p.id, p._cancel.is_some())).collect();
                match settled_tx.send(summary) {
                    Ok(()) | Err(_) => {}
                }
            });
        }
        let summary: Vec<(u64, bool)> = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&settled_rx)
            .unwrap_or_else(|_| {
                panic!("settle returned for {request_kind}: it did not deliver under the lock")
            });
        {
            let shared = hub.lock();
            assert_eq!(
                shared.replies.len(),
                1,
                "one failure was delivered for {request_kind}"
            );
            let (reply_id, reply) = &shared.replies[0];
            assert_eq!(*reply_id, id, "the failure is for the call");
            match reply {
                Reply::Query(Err((code, _))) if request_kind == "callback" => {
                    assert_eq!(*code, contract::ErrorCode::IoFailed);
                }
                Reply::Lock(Err(crate::host::LockError::Coded((code, _))))
                    if request_kind == "lock" =>
                {
                    assert_eq!(*code, contract::ErrorCode::IoFailed);
                }
                Reply::Http(_)
                | Reply::Drive(_)
                | Reply::Exec(_)
                | Reply::Model(_)
                | Reply::Delegate(_)
                | Reply::Query(_)
                | Reply::Lock(_)
                | Reply::Ask(_)
                | Reply::Tool(_)
                | Reply::Slept => panic!("no failure was delivered for {request_kind}"),
            }
        }
        assert_eq!(
            summary,
            vec![(id, false)],
            "a failed start parks no cancel handle for {request_kind}"
        );
    }
}

/// A failed spawn ends the call it was for: `spawn_failed` drops exactly the
/// callback parked for that call, fails the call with the step's I/O error,
/// and for an exec also clears its admission. Its body replaced by `()`
/// leaves the callback parked and the call unfinished, failing here. A real
/// thread spawn cannot be forced to fail from a test, so the private function
/// is called directly with the error a failed spawn returns.
#[test]
fn a_failed_spawn_drops_its_parked_callback_and_fails_the_call() {
    let dir = fakes::TempDir::new("fiber-schedule-spawn-failed");
    for exec in [false, true] {
        let lua = mlua::Lua::new();
        let hub = ready_hub();
        let id = started(&hub);
        let other = started(&hub);
        let mut parked = vec![entry(&lua, id), entry(&lua, other)];
        let mut shared = hub.lock();
        let _rx = exec.then(|| shared.register_exec(id));
        spawn_failed(
            &hub,
            &mut parked,
            shared,
            id,
            dir.path(),
            std::io::Error::other("injected"),
            exec,
        );
        assert_eq!(
            ids(&parked),
            [other],
            "only the failed call's callback is dropped (exec={exec})"
        );
        let now = hub.clock().now();
        let mut shared = hub.lock();
        assert!(
            !shared.exec_pending(id),
            "the admission is cleared (exec={exec})"
        );
        match shared.judge_tool("fiber.test/t", id, &Target::Tool("t".to_owned()), now, now) {
            (Some(super::super::Next::Return(Err(Error::Io { path, .. }))), _) => {
                assert_eq!(
                    path,
                    dir.path(),
                    "the call fails with the spawn's I/O error"
                );
            }
            _ => panic!("the call fails with an I/O error (exec={exec})"),
        }
    }
}

/// Commands for the settle-hook tests: `arm` sets a timer whose callback
/// finishes at once, `arm_exec` sets one whose callback suspends on a
/// `host.exec` run, and `plain` is a command that finishes. The `arm_exec`
/// child signals on the `ready` FIFO, then blocks reading the `block` FIFO
/// until the suspend test releases it, so the suspended state is a pause
/// point rather than a window. Tests that never run `arm_exec` format in
/// paths that are never opened.
fn timer_init(ready: &std::path::Path, block: &std::path::Path) -> String {
    format!(
        r#"fiber.command("arm", {{ timeout = 5000, run = function()
  host.after(100, function() return "fired" end, {{ timeout = 5000 }})
  return "armed" end }})
fiber.command("arm_exec", {{ timeout = 5000, run = function()
  host.after(100, function() host.exec("sh", {{ "-c", "echo $$ > {ready}; read _ < {block}; exit 0" }}) return "fired" end, {{ timeout = 5000 }})
  return "armed" end }})
fiber.command("plain", {{ timeout = 5000, run = function() return "plain" end }})
"#,
        ready = shell_quote(ready),
        block = shell_quote(block),
    )
}

/// Single-quotes `path` for the `sh -c` line in [`timer_init`].
// debt: third copy across three test crates (ceiling three); upgrade
// trigger: a fourth copy, or the next edit to any of them, moves it into
// fakes.
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Commands for the gated-hold test: `arm_gated` sets a timer whose
/// callback suspends on a `host.exec` run. The child signals on the
/// `ready` FIFO, then waits on the `gate` FIFO between its signal and its
/// `read _ < block`, so the test's release lands before the child's
/// `block` open whatever the scheduling.
fn gated_timer_init(
    ready: &std::path::Path,
    gate: &std::path::Path,
    block: &std::path::Path,
) -> String {
    format!(
        r#"fiber.command("arm_gated", {{ timeout = 5000, run = function()
  host.after(100, function() host.exec("sh", {{ "-c", "echo $$ > {ready}; read _ < {gate}; read _ < {block}; exit 0" }}) return "fired" end, {{ timeout = 5000 }})
  return "armed" end }})
"#,
        ready = shell_quote(ready),
        gate = shell_quote(gate),
        block = shell_quote(block),
    )
}

/// The settle hook's log: each pause as its target's name and stage.
type PauseLog = Arc<std::sync::Mutex<Vec<(String, Pause)>>>;

/// Installs a settle hook that logs every pause and signals each
/// `TimerDone`. The hook is process-wide, and nextest runs each test in its
/// own process, so no other test shares it.
fn log_pauses() -> (PauseLog, std::sync::mpsc::Receiver<()>) {
    let log: PauseLog = Arc::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let sink = Arc::clone(&log);
    pause_settle(Arc::new(move |target: &Target, pause: Pause| {
        sink.lock().unwrap().push((target.to_string(), pause));
        if pause == Pause::TimerDone {
            let _sent = tx.send(());
        }
    }));
    (log, rx)
}

/// The `TimerDone` entries of `log`, in order.
fn timer_done(log: &PauseLog) -> Vec<(String, Pause)> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(_, pause)| *pause == Pause::TimerDone)
        .cloned()
        .collect()
}

/// A hub in registration, with [`timer_init`] served on a thread; the call
/// `name` is queued and its id returned.
fn serve_timer_init(
    clock: &Arc<fakes::clock::FakeClock>,
    dir: &std::path::Path,
    name: &str,
    ready: &std::path::Path,
    block: &std::path::Path,
) -> (Arc<Hub>, u64) {
    serve_init(clock, dir, name, &timer_init(ready, block))
}

/// A hub in registration, with [`gated_timer_init`] served on a thread;
/// the call `name` is queued and its id returned.
fn serve_gated_timer_init(
    clock: &Arc<fakes::clock::FakeClock>,
    dir: &std::path::Path,
    name: &str,
    ready: &std::path::Path,
    gate: &std::path::Path,
    block: &std::path::Path,
) -> (Arc<Hub>, u64) {
    serve_init(clock, dir, name, &gated_timer_init(ready, gate, block))
}

/// A hub in registration, with `init` served on a thread; the call `name`
/// is queued and its id returned.
fn serve_init(
    clock: &Arc<fakes::clock::FakeClock>,
    dir: &std::path::Path,
    name: &str,
    init: &str,
) -> (Arc<Hub>, u64) {
    let init = init.to_owned();
    std::fs::write(dir.join("init.lua"), init).unwrap();
    let hub = Hub::new(clock.clone());
    hub.lock().phase = Phase::Registering { abandon_at: None };
    let id = hub.lock().push(
        Target::Command(name.to_owned()),
        serde_json::Value::Null,
        hub.clock().now(),
    );
    let (thread_hub, start) = (Arc::clone(&hub), start_in(dir));
    std::thread::spawn(move || serve(thread_hub, start));
    (hub, id)
}

/// Waits until the call `id` has returned.
fn wait_returned(hub: &Hub, id: u64) {
    let shared = hub.lock();
    assert!(
        hub.wait_for(shared, fakes::MUST_SUCCEED_WITHIN, |shared| {
            matches!(
                shared.calls.get(&id),
                Some(super::super::hub::Progress::Done(_))
            )
        }),
        "call {id} returns"
    );
}

/// A timer that finishes records `TimerDone` for its own target, and only
/// that. Without the guard the hook never fires, and with a flipped guard
/// it would also fire for a command, which case 3 asserts against.
#[test]
fn a_finished_timer_records_timer_done_for_its_own_target() {
    let dir = fakes::TempDir::new("fiber-schedule-timer-done");
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let (log, done) = log_pauses();
    let _guard = SettleGuard;
    let (hub, armed) = serve_timer_init(
        &clock,
        dir.path(),
        "arm",
        &dir.path().join("ready.fifo"),
        &dir.path().join("block.fifo"),
    );
    wait_returned(&hub, armed);
    let due = clock.origin() + Duration::from_millis(100);
    assert!(
        clock.await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the timer waits for its due time"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the timer callback completed");
    assert_eq!(
        timer_done(&log),
        vec![(Target::Timer { id: 0 }.to_string(), Pause::TimerDone)]
    );
}

/// The suspend test's hold on the timer callback's `sh`: the block FIFO
/// open read-write, so the child's `read` parks until the test releases
/// it. Dropping without `release` closes the end, so a failure before the
/// release still lets the child read end-of-file and exit instead of
/// hanging the run.
struct HoldExec {
    file: std::fs::File,
}

impl HoldExec {
    /// Opens `block` read-write: the open never blocks, and the held write
    /// end keeps the child's `read` parked.
    fn hold(block: &std::path::Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(block)
            .unwrap();
        Self { file }
    }

    /// Wakes the child's `read` with a line and keeps the end open: the
    /// buffered line survives until the child reads it, even when the
    /// release runs before the child's open. The end closes when the value
    /// drops, so a failure before the release still lets the child read
    /// end-of-file and exit instead of hanging the run.
    fn release(&mut self) {
        use std::io::Write as _;
        match writeln!(&mut self.file) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// A timer whose callback suspends records no `TimerDone` while it waits
/// on its `host.exec` run, and records exactly one once the callback
/// finishes. Without the guard's suspend check, the hook would fire at the
/// suspend.
#[test]
fn a_suspended_timer_records_no_timer_done_until_it_finishes() {
    let dir = fakes::TempDir::new("fiber-schedule-timer-suspend");
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let (log, done) = log_pauses();
    let _guard = SettleGuard;
    // The ready reader is open before the child runs, so its signal line
    // is never lost and its open never blocks; the held write end keeps
    // the child's `read` parked until the release below.
    let ready = fakes::children::Ready::new(dir.path());
    let block = dir.path().join("block.fifo");
    fakes::fifo(&block);
    let mut hold = HoldExec::hold(&block);
    let (hub, armed) = serve_timer_init(&clock, dir.path(), "arm_exec", ready.path(), &block);
    wait_returned(&hub, armed);
    let due = clock.origin() + Duration::from_millis(100);
    assert!(
        clock.await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the timer waits for its due time"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    // The child reached its script, so the suspension below stays put:
    // nothing advances the fake clock while the exec is held.
    let started = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    {
        let shared = hub.lock();
        assert!(
            hub.wait_for(shared, fakes::MUST_SUCCEED_WITHIN, |shared| {
                shared.calls.values().any(|progress| {
                    matches!(
                        progress,
                        super::super::hub::Progress::Started {
                            run: Run::Parked,
                            ..
                        }
                    )
                })
            }),
            "the timer callback suspends on its exec"
        );
    }
    // A host call suspends the callback until the reply arrives
    // (`docs/extensions.md`, "How an extension runs"), so the suspended
    // timer has not finished and settles nothing yet.
    assert!(timer_done(&log).is_empty(), "no TimerDone while suspended");
    hold.release();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the timer callback completed");
    // Each callback finishes before the next starts (`docs/extensions.md`,
    // "How an extension runs"), so the released timer settles exactly once.
    assert_eq!(
        timer_done(&log),
        vec![(Target::Timer { id: 0 }.to_string(), Pause::TimerDone)]
    );
    assert!(
        fakes::group_empties(
            started[0],
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the released exec left its group behind"
    );
}

/// A release before the child opens `block` still completes the timer: the
/// timer callback's child signals on the `ready` FIFO, then waits on a
/// second FIFO (`gate`) between its signal and its `read _ < block`, so the
/// test's release lands before the child's `block` open whatever the
/// scheduling, and the settle hook records exactly one `TimerDone`. Without
/// the held end staying open, the child's `block` open blocks with no
/// writer and the completion wait below expires.
#[test]
fn a_released_hold_is_not_lost_before_the_child_opens_the_fifo() {
    use std::io::Write as _;

    let dir = fakes::TempDir::new("fiber-schedule-hold-release");
    // The watchdog kills the gated child and its group when the test
    // fails before completion: dropping the hold cannot release the gate
    // FIFO, and the frozen fake clock cannot expire the exec.
    let watchdog = fakes::Watchdog::matching(&dir.path().display().to_string());
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let (log, done) = log_pauses();
    let _guard = SettleGuard;
    // The ready reader is open before the child runs, so its signal line
    // is never lost and its open never blocks; the held write end keeps
    // the child's `read` parked until the release below.
    let ready = fakes::children::Ready::new(dir.path());
    let gate = dir.path().join("gate.fifo");
    let block = dir.path().join("block.fifo");
    for fifo in [&gate, &block] {
        fakes::fifo(fifo);
    }
    let mut hold = HoldExec::hold(&block);
    let (hub, armed) =
        serve_gated_timer_init(&clock, dir.path(), "arm_gated", ready.path(), &gate, &block);
    wait_returned(&hub, armed);
    let due = clock.origin() + Duration::from_millis(100);
    assert!(
        clock.await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the timer waits for its due time"
    );
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(100),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    // The child reached its script, so it is at or on its way to the gate:
    // the gate read comes before the block open, whatever the scheduling.
    let started = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    {
        let shared = hub.lock();
        assert!(
            hub.wait_for(shared, fakes::MUST_SUCCEED_WITHIN, |shared| {
                shared.calls.values().any(|progress| {
                    matches!(
                        progress,
                        super::super::hub::Progress::Started {
                            run: Run::Parked,
                            ..
                        }
                    )
                })
            }),
            "the timer callback suspends on its exec"
        );
    }
    // A host call suspends the callback until the reply arrives
    // (`docs/extensions.md`, "How an extension runs"), so the suspended
    // timer has not finished and settles nothing yet.
    assert!(timer_done(&log).is_empty(), "no TimerDone while suspended");
    // The release lands before the gate opens, and the child's block open
    // comes after its gate read, so the release lands before the open.
    hold.release();
    // Opening read-write never blocks, and the buffered line lets the
    // child's gate read finish so it reaches the block open after the
    // release.
    let mut gate_hold = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&gate)
        .unwrap();
    gate_hold.write_all(b"\n").unwrap();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("the timer callback completed");
    // Each callback finishes before the next starts (`docs/extensions.md`,
    // "How an extension runs"), so the released timer settles exactly once.
    assert_eq!(
        timer_done(&log),
        vec![(Target::Timer { id: 0 }.to_string(), Pause::TimerDone)]
    );
    assert!(
        fakes::group_empties(
            started[0],
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the released exec left its group behind"
    );
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

/// A non-timer call that finishes records no `TimerDone`. Without the
/// timer check, a flipped `&&` would fire the hook for this command.
#[test]
fn a_finished_command_records_no_timer_done() {
    let dir = fakes::TempDir::new("fiber-schedule-command-done");
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let (log, _done) = log_pauses();
    let _guard = SettleGuard;
    let (hub, plain) = serve_timer_init(
        &clock,
        dir.path(),
        "plain",
        &dir.path().join("ready.fifo"),
        &dir.path().join("block.fifo"),
    );
    wait_returned(&hub, plain);
    assert!(
        timer_done(&log).is_empty(),
        "a finished command is no timer"
    );
}

/// A `HostDelegate` for the abandon tests: `start` reports its call and
/// waits for the test's answer, `cancel` records the ids and whether the
/// hub lock was free when it ran. A cancel under the hub lock would find
/// the lock held; every abandon path drops its `Unsent` after it releases
/// the lock, so the cancel always finds it free.
struct AbandonDelegate {
    hub: std::sync::Mutex<Option<std::sync::Weak<Hub>>>,
    arrived: std::sync::mpsc::Sender<(String, serde_json::Map<String, serde_json::Value>)>,
    answer: std::sync::Mutex<std::sync::mpsc::Receiver<DelegateAnswer>>,
    cancels: std::sync::Mutex<Vec<Vec<contract::JobId>>>,
    free_at_cancel: std::sync::Mutex<Vec<bool>>,
    starts: std::sync::atomic::AtomicUsize,
}

/// What a held `start` answers once the test releases it: the started id,
/// which the fake binds first, or a failure, likewise after a bind.
enum DelegateAnswer {
    Id(contract::JobId),
    Fail(contract::JobId, contract::shapes::Failure),
}

impl contract::extension::HostDelegate for AbandonDelegate {
    fn start(
        &self,
        extension: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        bind: &mut dyn FnMut(&contract::JobId),
    ) -> Result<contract::JobId, contract::shapes::Failure> {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.arrived
            .send((extension.to_owned(), arguments.clone()))
            .expect("the test waits for the start");
        match self
            .answer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv()
        {
            Ok(DelegateAnswer::Id(id)) => {
                bind(&id);
                Ok(id)
            }
            Ok(DelegateAnswer::Fail(id, failure)) => {
                bind(&id);
                Err(failure)
            }
            Err(_) => panic!("the test answers every start it waits for"),
        }
    }

    fn cancel(&self, ids: &[contract::JobId]) -> usize {
        let free = self
            .hub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|hub| hub.upgrade())
            .is_some_and(|hub| hub.try_lock().is_some());
        self.free_at_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(free);
        self.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ids.to_vec());
        ids.len()
    }

    fn retain(&self, _loaded: &[String]) -> usize {
        0
    }
}

/// A `start` the fake reports: the extension's name and its arguments.
type DelegateArrival = (String, serde_json::Map<String, serde_json::Value>);

/// The `Reply::Delegate` from `reply_rx`, failing the test on any other
/// reply: every delegate-worker test below waits on exactly one.
#[track_caller]
fn delegate_reply(
    reply_rx: &std::sync::mpsc::Receiver<crate::host::Reply>,
    what: &str,
) -> Result<contract::JobId, (contract::ErrorCode, String)> {
    match fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(reply_rx)
        .expect("the reply arrives")
    {
        crate::host::Reply::Delegate(outcome) => outcome,
        crate::host::Reply::Http(_)
        | crate::host::Reply::Ask(_)
        | crate::host::Reply::Drive(_)
        | crate::host::Reply::Exec(_)
        | crate::host::Reply::Tool(_)
        | crate::host::Reply::Model(_)
        | crate::host::Reply::Query(_)
        | crate::host::Reply::Lock(_)
        | crate::host::Reply::Slept => panic!("{what}"),
    }
}

/// Watches `hub`'s delegates with a fresh fake: sets it as the service and
/// records `j_1` and `j_2` as bound. Returns the fake with the channels its
/// `start` reports and waits on.
fn watch_delegates(
    hub: &Arc<Hub>,
) -> (
    Arc<AbandonDelegate>,
    std::sync::mpsc::Receiver<DelegateArrival>,
    std::sync::mpsc::Sender<DelegateAnswer>,
) {
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let fake = Arc::new(AbandonDelegate {
        hub: std::sync::Mutex::new(Some(Arc::downgrade(hub))),
        arrived: arrived_tx,
        answer: std::sync::Mutex::new(answer_rx),
        cancels: std::sync::Mutex::new(Vec::new()),
        free_at_cancel: std::sync::Mutex::new(Vec::new()),
        starts: std::sync::atomic::AtomicUsize::new(0),
    });
    hub.set_delegate(fake.clone());
    {
        let mut shared = hub.lock();
        shared.delegates.insert(
            contract::JobId("j_1".to_owned()),
            (1, Duration::from_secs(5)),
        );
        shared.delegates.insert(
            contract::JobId("j_2".to_owned()),
            (2, Duration::from_secs(5)),
        );
    }
    (fake, arrived_rx, answer_tx)
}

/// The ids each cancel call carried, sorted: one abandon cancels every
/// binding it took, in one call.
fn cancelled(fake: &AbandonDelegate) -> Vec<Vec<String>> {
    let cancels = fake
        .cancels
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cancels
        .iter()
        .map(|ids| {
            let mut names: Vec<String> = ids.iter().map(|id| id.0.clone()).collect();
            names.sort();
            names
        })
        .collect()
}

/// Every cancel ran after the hub lock was released, never under it.
fn assert_cancelled_free(fake: &AbandonDelegate, bound: &[&str]) {
    let expected: Vec<String> = bound.iter().map(|name| name.to_string()).collect();
    assert_eq!(
        cancelled(fake),
        vec![expected],
        "the abandon cancels every binding in one call"
    );
    assert!(
        fake.free_at_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .all(|free| *free),
        "the cancel runs after the hub lock is released"
    );
}

/// An abandon past the grace of the watched hook cancels the hub's bound
/// delegates, and fails the caller abandoned.
#[test]
fn an_abandoned_watch_cancels_its_bound_delegates() {
    use super::super::hub::Next;

    let (hub, _watched, target, _timeout, deadline) = armed_hook();
    let fake = watch_delegates(&hub).0;
    let at = deadline
        .checked_add(GRACE)
        .and_then(|at| at.checked_add(Duration::from_millis(1)))
        .unwrap();
    let unsent = {
        let mut shared = hub.lock();
        let probe = shared.push(target.clone(), serde_json::Value::Null, at);
        match shared.judge("ext", probe, &target, at, at) {
            (Next::Return(Err(Error::Abandoned { .. })), unsent) => unsent,
            _ => panic!("the armed watch past its grace abandons the VM"),
        }
    };
    drop(unsent);
    assert_cancelled_free(&fake, &["j_1", "j_2"]);
}

/// A hook still on the thread past its grace abandons the VM and cancels
/// its bound delegates, even with no watch armed before this judge.
#[test]
fn a_hook_running_past_its_grace_cancels_its_bound_delegates() {
    use super::super::declared::DeclaredCommand;
    use super::super::hub::Next;

    let hub = Hub::new(fakes::clock::FakeClock::new());
    let target = Target::Command("spin".to_owned());
    let timeout = Duration::from_millis(50);
    let now = hub.clock().now();
    let asked = now.checked_sub(timeout).unwrap();
    let deadline = asked.checked_add(timeout).unwrap();
    let fake = watch_delegates(&hub).0;
    let unsent = {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        if let Phase::Ready(timeouts) = &mut shared.phase {
            timeouts.commands.insert(
                "spin".to_owned(),
                DeclaredCommand {
                    timeout,
                    description: String::new(),
                },
            );
        } else {
            panic!("not ready");
        }
        let id = shared.push(target.clone(), serde_json::Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            id,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        let at = deadline
            .checked_add(GRACE)
            .and_then(|at| at.checked_add(Duration::from_millis(1)))
            .unwrap();
        match shared.judge("ext", id, &target, asked, at) {
            (Next::Return(Err(Error::Abandoned { .. })), unsent) => unsent,
            _ => panic!("the hook past its grace abandons the VM"),
        }
    };
    drop(unsent);
    assert_cancelled_free(&fake, &["j_1", "j_2"]);
}

/// A search backend still on the thread past its grace is abandoned with
/// `Stopped`, and still cancels its bound delegates: the cancel is keyed
/// on the abandon site, not the reason.
#[test]
fn a_search_running_past_its_grace_cancels_its_bound_delegates() {
    use super::super::hub::Next;

    let hub = Hub::new(fakes::clock::FakeClock::new());
    let target = Target::Search("s".to_owned());
    let timeout = Duration::from_millis(50);
    let now = hub.clock().now();
    let asked = now.checked_sub(timeout).unwrap();
    let deadline = asked.checked_add(timeout).unwrap();
    let fake = watch_delegates(&hub).0;
    let unsent = {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        if let Phase::Ready(timeouts) = &mut shared.phase {
            timeouts.search.insert("s".to_owned(), timeout);
        } else {
            panic!("not ready");
        }
        let id = shared.push(target.clone(), serde_json::Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            id,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        let at = deadline
            .checked_add(GRACE)
            .and_then(|at| at.checked_add(Duration::from_millis(1)))
            .unwrap();
        match shared.judge("ext", id, &target, asked, at) {
            (Next::Return(Err(Error::Abandoned { .. })), unsent) => unsent,
            _ => panic!("the search past its grace abandons the VM"),
        }
    };
    drop(unsent);
    assert_cancelled_free(&fake, &["j_1", "j_2"]);
}

/// A provider load still running past its abandonment cancels the hub's
/// bound delegates.
#[test]
fn a_provider_load_past_its_abandonment_cancels_its_bound_delegates() {
    use super::super::hub::{Next, ProviderLoad};

    let hub = Hub::new(fakes::clock::FakeClock::new());
    let fake = watch_delegates(&hub).0;
    let now = hub.clock().now();
    let asked = now.checked_sub(Duration::from_millis(50)).unwrap();
    let unsent = {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        shared.loads.insert(
            "p".to_owned(),
            ProviderLoad::Running {
                abandon_at: Some(asked),
            },
        );
        match shared.judge_load("ext", "p", asked, now) {
            (Next::Return(Err(Error::Abandoned { .. })), unsent) => unsent,
            _ => panic!("the load past its abandonment abandons the VM"),
        }
    };
    drop(unsent);
    assert_cancelled_free(&fake, &["j_1", "j_2"]);
}

/// `dispose` stops the hub but cancels no bound delegate: a reload, a
/// removal and a session end keep their own stop rules.
#[test]
fn dispose_cancels_no_bound_delegate() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let fake = watch_delegates(&hub).0;
    hub.dispose("ext");
    assert!(
        fake.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "dispose cancels nothing"
    );
}

/// A hub ready for a `host.delegate` admit from `target`: phase ready, the
/// target's timeout declared, an inbox set and the fake as its service.
fn delegate_rig(
    target: Target,
    timeout: Duration,
) -> (
    Arc<Hub>,
    Arc<AbandonDelegate>,
    std::sync::mpsc::Receiver<DelegateArrival>,
    std::sync::mpsc::Sender<DelegateAnswer>,
    fakes::TempDir,
) {
    use super::super::declared::{DeclaredCommand, DeclaredHook, HookPhase};

    let dir = fakes::TempDir::new("fiber-schedule-delegate");
    let hub = Hub::new(fakes::clock::FakeClock::new());
    {
        let mut shared = hub.lock();
        shared.phase = Phase::Ready(Default::default());
        if let Phase::Ready(timeouts) = &mut shared.phase {
            match &target {
                Target::Command(name) => {
                    timeouts.commands.insert(
                        name.clone(),
                        DeclaredCommand {
                            timeout,
                            description: String::new(),
                        },
                    );
                }
                Target::Hook { point, .. } => {
                    timeouts.hooks.by_point.insert(
                        point.clone(),
                        vec![DeclaredHook {
                            phase: HookPhase::Transform,
                            blocking: false,
                            timeout,
                            entry: None,
                            phase_explicit: false,
                        }],
                    );
                }
                Target::Tool(name) | Target::Effects(name) => {
                    timeouts.tools.insert(
                        name.clone(),
                        super::super::tool::DeclaredTool {
                            name: name.clone(),
                            description: String::new(),
                            input_schema: serde_json::Value::Null,
                            effects: None,
                            timeout,
                            keep: contract::tool::Bound::DEFAULT,
                        },
                    );
                }
                Target::Search(name) => {
                    timeouts.search.insert(name.clone(), timeout);
                }
                // A delegate run needs no registration: its timeout
                // comes from the yield.
                Target::Timer { .. }
                | Target::Provider { .. }
                | Target::Watcher { .. }
                | Target::Delegate { .. } => {}
            }
        } else {
            panic!("not ready");
        }
    }
    let (inbox_tx, _inbox_rx) = std::sync::mpsc::channel();
    hub.set_inbox(inbox_tx);
    let (fake, arrived_rx, answer_tx) = watch_delegates(&hub);
    // The worker tests bind their own ids; clear the rig's two.
    hub.lock().delegates.clear();
    (hub, fake, arrived_rx, answer_tx, dir)
}

/// Admits `Request::Delegate` from `target` on `hub` and runs its worker
/// to completion on the calling thread, without the hub lock held.
fn run_delegate_worker(
    hub: &Arc<Hub>,
    dir: &std::path::Path,
    target: Target,
    fake: &Arc<AbandonDelegate>,
    reply_tx: std::sync::mpsc::Sender<crate::host::Reply>,
) {
    use super::super::host_calls::{Admitted, Call, admit};

    let start = start_in(dir);
    let request = crate::host::Request::Delegate {
        arguments: serde_json::Map::new(),
        handle: 7,
        timeout: Duration::from_millis(100),
    };
    let deliver: crate::oauth::Deliver = Arc::new(move |reply| {
        reply_tx.send(reply).expect("the test waits for the reply");
    });
    let spawn = {
        let shared = hub.lock();
        match admit(
            &start,
            hub,
            shared,
            Call {
                id: 1,
                target: &target,
                deadline: None,
                http: None,
            },
            request,
            None,
            None,
            Some(fake.clone()),
            &deliver,
        ) {
            Admitted::Spawning { shared, spawn } => {
                drop(shared);
                spawn
            }
            Admitted::Replied(_) => panic!("the worker spawns"),
            Admitted::Asked { .. }
            | Admitted::Started(_)
            | Admitted::Sleep(_)
            | Admitted::Nothing => panic!("a delegate admit spawns its worker"),
        }
    };
    (spawn.work)();
}

/// An abandon between `bind` and the job's record cancels the started job
/// from the post-start check: the abandon's cancel found no job yet.
#[test]
fn an_abandon_between_bind_and_record_cancels_the_started_job() {
    let (hub, fake, arrived_rx, answer_tx, dir) = delegate_rig(
        Target::Command("go".to_owned()),
        Duration::from_millis(5_000),
    );
    let (checked_tx, checked_rx) = std::sync::mpsc::channel();
    pause_settle(Arc::new(move |_target, pause| {
        if pause == Pause::DelegateChecked {
            checked_tx.send(()).expect("the test waits for the check");
        }
    }));
    let _guard = SettleGuard;
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let worker = {
        let hub = Arc::clone(&hub);
        let fake = Arc::clone(&fake);
        let dir = dir.path().to_path_buf();
        std::thread::spawn(move || {
            run_delegate_worker(
                &hub,
                &dir,
                Target::Command("go".to_owned()),
                &fake,
                reply_tx,
            )
        })
    };
    let (extension, _arguments) = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrived_rx)
        .expect("the start arrives");
    assert_eq!(
        extension, "fiber.test/stopped",
        "the worker starts for its extension"
    );
    {
        let mut shared = hub.lock();
        let unsent = shared.abandon(StopReason::Abandoned {
            extension: "ext".to_owned(),
            callback: "command go".to_owned(),
        });
        drop(shared);
        drop(unsent);
    }
    answer_tx
        .send(DelegateAnswer::Id(contract::JobId("j_1".to_owned())))
        .expect("the held start is released");
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&checked_rx)
        .expect("the worker reaches its post-start check");
    worker.join().expect("the worker ends");
    assert_cancelled_free(&fake, &["j_1"]);
    assert_eq!(
        delegate_reply(&reply_rx, "the started reply still arrives")
            .unwrap()
            .0,
        "j_1"
    );
}

/// A start held across `dispose` is not cancelled: the hub was stopped,
/// not abandoned, so the start that completes across a reload keeps
/// running. The started reply still arrives.
#[test]
fn a_start_held_across_dispose_is_not_cancelled() {
    let (hub, fake, arrived_rx, answer_tx, dir) = delegate_rig(
        Target::Command("go".to_owned()),
        Duration::from_millis(5_000),
    );
    let (checked_tx, checked_rx) = std::sync::mpsc::channel();
    pause_settle(Arc::new(move |_target, pause| {
        if pause == Pause::DelegateChecked {
            checked_tx.send(()).expect("the test waits for the check");
        }
    }));
    let _guard = SettleGuard;
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let worker = {
        let hub = Arc::clone(&hub);
        let fake = Arc::clone(&fake);
        let dir = dir.path().to_path_buf();
        std::thread::spawn(move || {
            run_delegate_worker(
                &hub,
                &dir,
                Target::Command("go".to_owned()),
                &fake,
                reply_tx,
            )
        })
    };
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrived_rx)
        .expect("the start arrives");
    hub.dispose("ext");
    answer_tx
        .send(DelegateAnswer::Id(contract::JobId("j_1".to_owned())))
        .expect("the held start is released");
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&checked_rx)
        .expect("the worker reaches its post-start check");
    worker.join().expect("the worker ends");
    assert!(
        fake.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "no abandon means no cancel"
    );
    assert_eq!(
        delegate_reply(&reply_rx, "the started reply still arrives")
            .unwrap()
            .0,
        "j_1"
    );
}

/// A start that fails after `bind` leaves no binding for a later abandon
/// to cancel: the worker removes what the failed start recorded.
#[test]
fn a_start_that_fails_after_bind_leaves_no_binding() {
    let (hub, fake, arrived_rx, answer_tx, dir) = delegate_rig(
        Target::Command("go".to_owned()),
        Duration::from_millis(5_000),
    );
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    answer_tx
        .send(DelegateAnswer::Fail(
            contract::JobId("j_9".to_owned()),
            contract::shapes::Failure {
                code: contract::ErrorCode::ToolError,
                message: "ten delegates already run".to_owned(),
                retry_after_ms: None,
                provider: None,
            },
        ))
        .expect("the start is answered");
    run_delegate_worker(
        &hub,
        dir.path(),
        Target::Command("go".to_owned()),
        &fake,
        reply_tx,
    );
    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrived_rx)
        .expect("the start arrives");
    let (code, message) =
        delegate_reply(&reply_rx, "the failure arrives with its code and message").unwrap_err();
    assert_eq!(code, contract::ErrorCode::ToolError);
    assert_eq!(message, "ten delegates already run");
    assert!(
        hub.lock().delegates.is_empty(),
        "the failed start unbound its id"
    );
    assert!(
        fake.cancels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "no binding means no cancel"
    );
}

/// An abandoned hub answers a delegate start `closing` before it spawns
/// any worker: a leaked VM thread's start spawns nothing.
#[test]
fn an_abandoned_hub_refuses_a_delegate_start_closed() {
    use super::super::host_calls::{Admitted, Call, admit};

    let dir = fakes::TempDir::new("fiber-schedule-delegate-abandoned");
    let hub = ready_hub();
    let (fake, _arrived_rx, _answer_tx) = watch_delegates(&hub);
    hub.lock().delegates.clear();
    hub.lock().abandoned = true;
    let (inbox_tx, _inbox_rx) = std::sync::mpsc::channel();
    hub.set_inbox(inbox_tx);
    let target = Target::Hook {
        point: "before_message".to_owned(),
        index: 0,
    };
    let start = start_in(dir.path());
    let request = crate::host::Request::Delegate {
        arguments: serde_json::Map::new(),
        handle: 7,
        timeout: Duration::from_millis(100),
    };
    let deliver: crate::oauth::Deliver = Arc::new(|_| panic!("no reply is delivered"));
    let shared = hub.lock();
    match admit(
        &start,
        &hub,
        shared,
        Call {
            id: 1,
            target: &target,
            deadline: None,
            http: None,
        },
        request,
        None,
        None,
        Some(fake.clone()),
        &deliver,
    ) {
        Admitted::Replied(crate::host::Reply::Delegate(Err((code, message)))) => {
            assert_eq!(code, contract::ErrorCode::Closing);
            assert_eq!(message, "host.delegate: this extension was stopped");
        }
        Admitted::Asked { .. }
        | Admitted::Spawning { .. }
        | Admitted::Started(_)
        | Admitted::Sleep(_)
        | Admitted::Nothing
        | Admitted::Replied(_) => panic!("the abandoned hub answers closing"),
    }
    assert_eq!(
        fake.starts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no worker means no start call"
    );
}

/// A delegate start from a provider function, a search function or an
/// effects function is refused before any other check: each answers
/// `invalid_arguments` with zero `start` calls, both with a service set
/// and with none. A VM with no service would otherwise answer `closing`
/// from an effects function, hiding the real refusal.
#[test]
fn delegate_starts_are_refused_from_provider_search_and_effects() {
    use super::super::host_calls::{Admitted, Call, admit};

    let refused = [
        Target::Provider {
            name: "p".to_owned(),
            function: "credential",
            credential: None,
        },
        Target::Search("s".to_owned()),
        Target::Effects("e".to_owned()),
    ];
    for target in refused {
        for serviced in [true, false] {
            let dir = fakes::TempDir::new("fiber-schedule-delegate-refused");
            let hub = ready_hub();
            let (fake, _arrived_rx, _answer_tx) = watch_delegates(&hub);
            hub.lock().delegates.clear();
            let delegate =
                serviced.then(|| fake.clone() as Arc<dyn contract::extension::HostDelegate>);
            let (inbox_tx, _inbox_rx) = std::sync::mpsc::channel();
            hub.set_inbox(inbox_tx);
            let start = start_in(dir.path());
            let request = crate::host::Request::Delegate {
                arguments: serde_json::Map::new(),
                handle: 7,
                timeout: Duration::from_millis(100),
            };
            let deliver: crate::oauth::Deliver = Arc::new(|_| panic!("no reply is delivered"));
            let shared = hub.lock();
            match admit(
                &start,
                &hub,
                shared,
                Call {
                    id: 1,
                    target: &target,
                    deadline: None,
                    http: None,
                },
                request,
                None,
                None,
                delegate,
                &deliver,
            ) {
                Admitted::Replied(crate::host::Reply::Delegate(Err((code, message)))) => {
                    assert_eq!(code, contract::ErrorCode::InvalidArguments);
                    assert_eq!(message, "host.delegate: not available here");
                }
                Admitted::Asked { .. }
                | Admitted::Spawning { .. }
                | Admitted::Started(_)
                | Admitted::Sleep(_)
                | Admitted::Nothing
                | Admitted::Replied(_) => panic!("{target} refuses the delegate start"),
            }
            assert_eq!(
                fake.starts.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{target} reaches no start"
            );
        }
    }
}

/// A delegate start from a command, a hook, a timer or a tool call reaches
/// the service: the worker runs the start and delivers its id.
#[test]
fn delegate_starts_from_commands_hooks_timers_and_tools_reach_start() {
    for target in [
        Target::Command("go".to_owned()),
        Target::Hook {
            point: "before_message".to_owned(),
            index: 0,
        },
        Target::Timer { id: 0 },
        Target::Tool("t".to_owned()),
    ] {
        let (hub, fake, arrived_rx, answer_tx, dir) =
            delegate_rig(target.clone(), Duration::from_millis(5_000));
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        answer_tx
            .send(DelegateAnswer::Id(contract::JobId("j_1".to_owned())))
            .expect("the start is answered");
        run_delegate_worker(&hub, dir.path(), target.clone(), &fake, reply_tx);
        let (extension, _arguments) = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&arrived_rx)
            .expect("the start arrives");
        assert_eq!(extension, "fiber.test/stopped");
        let what = format!("{target} delivers the started id");
        assert_eq!(delegate_reply(&reply_rx, &what).unwrap().0, "j_1");
    }
}

/// `Unsent::from` keeps its deliveries until the `Unsent` drops: the
/// ack inside the delivery stays alive in the `Unsent` and drops with
/// it, so a `Default::default()` in its place fails the first assert.
#[test]
fn unsent_from_vec_keeps_its_deliveries_until_drop() {
    use super::super::hub::Unsent;
    use contract::inbox::{Ack, Delivery};

    let held = std::sync::Arc::new(());
    let weak = std::sync::Arc::downgrade(&held);
    let kept = std::sync::Arc::clone(&held);
    drop(held);
    let delivery = Delivery::Close(Ack(Box::new(move |_| drop(kept))));
    let unsent = Unsent::from(vec![delivery]);
    assert!(
        weak.upgrade().is_some(),
        "the conversion keeps its deliveries"
    );
    drop(unsent);
    assert!(
        weak.upgrade().is_none(),
        "dropping the Unsent drops its deliveries"
    );
}

/// A delegate starter for the `||` gate: never started in these tests.
struct LiveDelegate;

impl contract::extension::HostDelegate for LiveDelegate {
    fn start(
        &self,
        _extension: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
        _bind: &mut dyn FnMut(&contract::JobId),
    ) -> Result<contract::JobId, contract::shapes::Failure> {
        unreachable!("no start runs in these tests");
    }

    fn retain(&self, _loaded: &[String]) -> usize {
        0
    }

    fn cancel(&self, _jobs: &[contract::JobId]) -> usize {
        0
    }
}

/// The delegate starter answers only while live: a drop alone and a seal
/// alone each silence it, even for a service present under that single
/// flag. With the `||` flipped to `&&`, either alone would wrongly still
/// answer. The drop clears the service, so it is reinstalled directly
/// (`set_delegate` refuses after a drop) to isolate the gate; the seal
/// leaves `set_delegate` open, so a late service takes the public path.
#[test]
fn delegate_answers_only_while_live() {
    let service: Arc<dyn contract::extension::HostDelegate> = Arc::new(LiveDelegate);
    let live = Hub::new(fakes::clock::FakeClock::new());
    live.set_delegate(Arc::clone(&service));
    assert!(live.delegate().is_some(), "live answers");
    let hub = Hub::new(fakes::clock::FakeClock::new());
    hub.set_delegate(Arc::clone(&service));
    hub.dispose("ext");
    hub.lock().router.delegate = Some(Arc::clone(&service));
    assert!(
        hub.delegate().is_none(),
        "nothing starts after the drop alone"
    );
    let hub = Hub::new(fakes::clock::FakeClock::new());
    hub.seal();
    hub.set_delegate(Arc::clone(&service));
    assert!(
        hub.delegate().is_none(),
        "nothing starts after the seal alone"
    );
    assert!(
        Hub::new(fakes::clock::FakeClock::new())
            .delegate()
            .is_none(),
        "nothing starts before delegate_to"
    );
}

/// `bind_delegate` records its id, so a later abandon cancels it: with the
/// body replaced by `()` the binding stays missing and the abandon cancels
/// nothing.
#[test]
fn bind_delegate_records_its_id_for_a_later_abandon() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let (fake, _arrived_rx, _answer_tx) = watch_delegates(&hub);
    hub.lock().delegates.clear();
    let id = contract::JobId("j_bind".to_owned());
    hub.bind_delegate(id.clone(), 7, Duration::from_millis(100));
    assert_eq!(
        hub.lock().delegates.get(&id),
        Some(&(7, Duration::from_millis(100))),
        "bind records its id"
    );
    let unsent = {
        let mut shared = hub.lock();
        shared.abandon(StopReason::Abandoned {
            extension: "ext".to_owned(),
            callback: "hook".to_owned(),
        })
    };
    drop(unsent);
    assert_cancelled_free(&fake, &["j_bind"]);
}
