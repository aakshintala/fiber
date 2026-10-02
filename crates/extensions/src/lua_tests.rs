use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

/// A panic in a host function is never a Lua error the extension's `pcall`
/// can catch (`docs/code-quality.md`, "Panics"). Tests run under unwind,
/// where it reaches the Rust caller past the `pcall`; Fiber's builds abort at
/// the panic.
#[test]
fn a_panic_in_a_host_function_passes_the_extensions_pcall() {
    let vm = Vm::load(
        "fixture",
        &fakes::lua_fixture(),
        Path::new("/nonexistent-fiber-home"),
        &|_| Ok(()),
    )
    .unwrap();
    let boom = vm
        .lua
        .create_function(|_, ()| -> mlua::Result<()> { panic!("host bug") })
        .unwrap();
    vm.lua.globals().set("boom", boom).unwrap();
    let f = vm
        .lua
        .load(r#"local ok, e = pcall(boom) return tostring(ok) .. " " .. tostring(e)"#)
        .into_function()
        .unwrap();
    vm.deadline.start(LOAD_TIMEOUT);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        vm.resume(f, "boom", LOAD_TIMEOUT, mlua::Value::Nil)
    }));
    assert!(outcome.is_err(), "the panic became {outcome:?}");
}

/// A caller that stopped waiting abandoned the VM: the worker quits before it
/// runs the entry script or any later job.
#[test]
fn the_worker_quits_once_a_caller_stops_waiting() {
    let (inbox, jobs) = mpsc::channel();
    let (gone, _) = mpsc::channel();
    let (later, answers) = mpsc::channel();
    for reply in [gone, later] {
        inbox
            .send(schedule::Msg::Job(Job {
                target: Target::Command("echo".to_owned()),
                arg: Value::String("x".to_owned()),
                reply,
            }))
            .unwrap();
    }
    let http = inbox.clone();
    drop(inbox);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        schedule::serve(
            "fixture",
            &fakes::lua_fixture(),
            Path::new("/nonexistent-fiber-home"),
            &jobs,
            &http,
        );
        // The job left in the inbox is dropped with it, and its reply sender.
        drop(jobs);
        assert!(done_tx.send(()).is_ok());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "the worker did not quit"
    );
    assert_eq!(
        answers.recv_timeout(Duration::from_secs(2)).err(),
        Some(mpsc::RecvTimeoutError::Disconnected),
        "the later job was served"
    );
}

/// [`Deadline::at`] is the instant [`Deadline::start`] stored.
#[test]
fn the_deadline_reports_the_instant_it_was_started() {
    let deadline = Deadline::default();
    let started = deadline.start(Duration::from_secs(5));
    assert_eq!(deadline.at(), started);
}
