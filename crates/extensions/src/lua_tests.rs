use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

/// A panic in a host function is never a Lua error the extension's `pcall`
/// can catch (`docs/code-quality.md`, "Panics"). Tests run under unwind,
/// where it reaches the Rust caller past the `pcall`; Fiber's builds abort at
/// the panic.
#[test]
fn a_panic_in_a_host_function_passes_the_extensions_pcall() {
    let vm = Vm::load("fixture", &fakes::lua_fixture(), &|_| Ok(())).unwrap();
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
    let outcome = catch_unwind(AssertUnwindSafe(|| vm.resume(f, "boom", LOAD_TIMEOUT, "")));
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
            .send(Job {
                command: "echo".to_owned(),
                text: "x".to_owned(),
                reply,
            })
            .unwrap();
    }
    drop(inbox);
    serve("fixture", &fakes::lua_fixture(), &jobs);
    // Dropping the inbox drops the job left in it, and its reply sender.
    drop(jobs);
    assert_eq!(
        answers.recv_timeout(Duration::from_secs(10)).err(),
        Some(mpsc::RecvTimeoutError::Disconnected),
        "the later job was served"
    );
}
