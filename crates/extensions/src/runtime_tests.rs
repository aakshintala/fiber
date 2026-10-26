//! The backend-neutral callback owner (`super::runtime`): generation-keyed
//! function references and dispatch to Lua or process functions without
//! the caller naming a backend.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;
use std::time::Duration;

use contract::tool::Cancel;
use contract::{CommandId, RequestId};
use serde_json::{Value, json};

use super::runtime::{FunctionRef, FunctionTarget, LuaBinding, LuaKind, ProcessInvoke, Runtime};
use super::test_support::Home;
use crate::Error;

fn runtime() -> Runtime {
    Runtime::new(fakes::clock::FakeClock::new())
}

fn command(id: &str) -> CommandId {
    CommandId(id.to_owned())
}

fn handler(value: Value) -> ProcessInvoke {
    Arc::new(move |_args: Value, _cancel: &dyn Cancel| Ok(Some(value.clone())))
}

fn failing(message: &str) -> ProcessInvoke {
    let message = message.to_owned();
    Arc::new(move |_args: Value, _cancel: &dyn Cancel| {
        Err(Error::Lua {
            extension: "fiber.test/proc".to_owned(),
            message: message.clone(),
        })
    })
}

/// Registers `fiber.test/proc` as a process owner with one `run` function
/// answering `value`, and returns its reference.
fn process_run(runtime: &Runtime, value: Value) -> FunctionRef {
    runtime.register_owner("fiber.test/proc", None);
    runtime.install_process_handler("fiber.test/proc", command("c1"), "run", handler(value))
}

/// A generation starts at one and bumps on every re-registration, for both
/// backends. Dropping the bump would let a replacement share the old
/// generation, so the check below pins the exact numbers.
#[test]
fn generations_start_at_one_and_bump_per_registration() {
    let runtime = runtime();
    assert_eq!(runtime.register_owner("fiber.test/proc", None), 1);
    assert_eq!(runtime.register_owner("fiber.test/proc", None), 2);
    let home = Home::new();
    home.install("lua", "fiber.hook(\"turn_end\", { phase = \"transform\", on_failure = \"non-blocking\", timeout = 5000, run = function(input) return {} end })");
    let session = home.load(&[]);
    let extension = session.lua_extension("fiber.test/lua").unwrap();
    assert_eq!(
        runtime.register_owner("fiber.test/lua", Some(&extension)),
        1
    );
    assert_eq!(
        runtime.register_owner("fiber.test/lua", Some(&extension)),
        2
    );
}

/// A process function resolves within the generation that registered it.
/// Flipping the generation comparison to `!=` would resolve nothing, and
/// dropping it would resolve every generation.
#[test]
fn process_function_resolves_within_its_generation() {
    let runtime = runtime();
    let made = process_run(&runtime, json!({"ok": true}));
    assert_eq!(made.generation, 1);
    let resolved = runtime.resolve(&made).unwrap();
    assert_eq!(resolved.owner, "fiber.test/proc");
    assert!(matches!(resolved.target, FunctionTarget::Process(_)));
}

/// The debug rendering names only the owner and the backend for
/// developers, never the function itself: an empty rendering would hide
/// both, and naming the function would leak it.
#[test]
fn resolved_function_debug_names_owner_and_backend() {
    let runtime = runtime();
    let made = process_run(&runtime, json!({"ok": true}));
    let resolved = runtime.resolve(&made).unwrap();
    assert_eq!(
        format!("{resolved:?}"),
        "ResolvedFunction { owner: \"fiber.test/proc\", backend: \"process\" }"
    );
}

/// A replacement bumps the generation: the old reference no longer
/// resolves, and a reference minted at the new generation does. Resolving
/// by command id and key alone would answer with the replacement's
/// function.
#[test]
fn replacement_generation_rejects_old_reference() {
    let runtime = runtime();
    let old = process_run(&runtime, json!({"generation": 1}));
    runtime.register_owner("fiber.test/proc", None);
    let old_error = runtime.resolve(&old).unwrap_err().to_string();
    assert!(
        old_error.contains("generation 1"),
        "unexpected stale error: {old_error}"
    );
    let current = runtime.register_function(
        "fiber.test/proc",
        command("c1"),
        "run".to_owned(),
        FunctionTarget::Process(handler(json!({"generation": 2}))),
    );
    assert_eq!(current.generation, 2);
    assert!(runtime.resolve(&current).is_ok());
    let future = FunctionRef {
        command: command("c1"),
        key: "run".to_owned(),
        generation: 99,
    };
    assert!(runtime.resolve(&future).is_err());
}

/// A reference that was never registered resolves to nothing, whatever its
/// generation claims. Matching on command id alone would admit it.
#[test]
fn unknown_function_never_resolves() {
    let runtime = runtime();
    runtime.register_owner("fiber.test/proc", None);
    let missing = FunctionRef {
        command: command("nope"),
        key: "run".to_owned(),
        generation: 1,
    };
    assert!(runtime.resolve(&missing).is_err());
    let wrong_key = runtime.register_function(
        "fiber.test/proc",
        command("c1"),
        "run".to_owned(),
        FunctionTarget::Process(handler(json!(null))),
    );
    assert!(wrong_key.generation == 1);
    let other_key = FunctionRef {
        command: command("c1"),
        key: "effects".to_owned(),
        generation: 1,
    };
    assert!(runtime.resolve(&other_key).is_err());
}

/// Registering a function for an owner that was never registered records
/// nothing resolvable: the stub generation never matches a real one.
/// Silently creating the owner would resolve it.
#[test]
fn registering_a_function_for_an_unknown_owner_never_resolves() {
    let runtime = runtime();
    let made = runtime.register_function(
        "fiber.test/ghost",
        command("c9"),
        "run".to_owned(),
        FunctionTarget::Process(handler(json!(null))),
    );
    assert!(runtime.resolve(&made).is_err());
    assert_eq!(runtime.owner_name(&made), None);
}

/// The owner name answers for a known reference and stays silent for an
/// unknown one, so notices can name the owner without resolving it.
#[test]
fn owner_name_reports_known_and_unknown() {
    let runtime = runtime();
    let made = process_run(&runtime, json!(null));
    assert_eq!(
        runtime.owner_name(&made),
        Some("fiber.test/proc".to_owned())
    );
    let missing = FunctionRef {
        command: command("nope"),
        key: "run".to_owned(),
        generation: 1,
    };
    assert_eq!(runtime.owner_name(&missing), None);
}

/// A process hook runs its handler and the caller decodes the answer, as a
/// Lua hook does. A handler answering nothing fails the hook: there is no
/// empty hook answer.
#[test]
fn process_hook_dispatch_applies_decode() {
    let runtime = runtime();
    let made = process_run(&runtime, json!({"seen": true}));
    let (value, lines, changed) = runtime
        .hook_with(&made, json!({}), |value| Ok((value, false)))
        .unwrap();
    assert_eq!(value, json!({"seen": true}));
    assert!(lines.is_empty());
    assert!(!changed);
    let empty = runtime.register_function(
        "fiber.test/proc",
        command("c2"),
        "run".to_owned(),
        FunctionTarget::Process(Arc::new(|_args: Value, _cancel: &dyn Cancel| Ok(None))),
    );
    assert!(
        runtime
            .hook_with(&empty, json!({}), |value| Ok((value, false)))
            .is_err()
    );
}

/// A process tool run returns what its handler answered, and nothing when
/// the handler answers nothing: the same contract `LuaTool` keeps.
#[test]
fn process_tool_run_returns_handler_value() {
    let runtime = runtime();
    let made = process_run(&runtime, json!({"content": "hi"}));
    let cancel = support::cancel::Token::new();
    assert_eq!(
        runtime.tool_run(&made, json!({}), &cancel).unwrap(),
        Some(json!({"content": "hi"}))
    );
    let empty = runtime.register_function(
        "fiber.test/proc",
        command("c2"),
        "run".to_owned(),
        FunctionTarget::Process(Arc::new(|_args: Value, _cancel: &dyn Cancel| Ok(None))),
    );
    assert_eq!(runtime.tool_run(&empty, json!({}), &cancel).unwrap(), None);
    let failed = runtime.register_function(
        "fiber.test/proc",
        command("c3"),
        "run".to_owned(),
        FunctionTarget::Process(failing("boom")),
    );
    assert!(runtime.tool_run(&failed, json!({}), &cancel).is_err());
}

/// A process tool asked with an inner-call runner still runs: the runner is
/// the model's path, and a process callback has no runner yet. Dropping the
/// call when a runner is present would break every asked tool call.
#[test]
fn process_tool_run_asking_runs_without_runner() {
    let runtime = runtime();
    let made = process_run(&runtime, json!({"content": "hi"}));
    let cancel = support::cancel::Token::new();
    assert_eq!(
        runtime
            .tool_run_asking(&made, json!({}), &cancel, None)
            .unwrap(),
        Some(json!({"content": "hi"}))
    );
}

/// Computed callbacks resolve like `run`: an effects function and a search
/// backend each answer through their own reference. Provider and harness
/// functions stay on `LuaExtension` directly, so the runtime carries no
/// dispatch for them.
#[test]
fn process_computed_callbacks_resolve_by_key() {
    let runtime = runtime();
    runtime.register_owner("fiber.test/proc", None);
    let cancel = support::cancel::Token::new();
    let effects = runtime.install_process_handler(
        "fiber.test/proc",
        command("c1"),
        "effects",
        handler(json!({"effects": ["reads"], "reversible": true})),
    );
    assert_eq!(
        runtime.tool_effects(&effects, json!({})).unwrap(),
        json!({"effects": ["reads"], "reversible": true})
    );
    let search = runtime.install_process_handler(
        "fiber.test/proc",
        command("c2"),
        "run",
        handler(json!([{"title": "t", "url": "https://example.com", "snippet": "s"}])),
    );
    assert_eq!(
        runtime
            .search_run(&search, json!({"query": "q"}), &cancel)
            .unwrap(),
        Some(json!([{"title": "t", "url": "https://example.com", "snippet": "s"}]))
    );
}

/// A stale reference fails every dispatch, and never selects the
/// replacement's function. Letting it through would run the wrong
/// generation's code.
#[test]
fn stale_reference_fails_every_dispatch() {
    let runtime = runtime();
    let old = process_run(&runtime, json!({"generation": 1}));
    runtime.register_owner("fiber.test/proc", None);
    let cancel = support::cancel::Token::new();
    assert!(runtime.resolve(&old).is_err());
    assert!(
        runtime
            .hook_with(&old, json!({}), |value| Ok((value, false)))
            .is_err()
    );
    assert!(runtime.tool_run(&old, json!({}), &cancel).is_err());
    assert!(runtime.tool_effects(&old, json!({})).is_err());
    assert!(runtime.search_run(&old, json!({}), &cancel).is_err());
    assert!(runtime.drain_watcher(&old, json!({})).is_err());
    assert!(!runtime.is_watcher_busy(&old));
    assert!(runtime.push_watcher(&old, json!({})).is_none());
    assert!(runtime.queue_command(&old, command("q"), "go").is_err());
}

/// A Lua hook dispatched through the runtime answers exactly as a direct
/// call does: the runtime forwards, it does not reinterpret.
#[test]
fn lua_hook_dispatch_matches_direct_call() {
    let home = Home::new();
    home.install(
        "hook",
        "fiber.hook(\"turn_end\", { phase = \"transform\", on_failure = \"non-blocking\", timeout = 5000, run = function(input) return { seen = true } end })",
    );
    let session = home.load(&[]);
    let extension = session.lua_extension("fiber.test/hook").unwrap();
    let runtime = runtime();
    runtime.register_owner("fiber.test/hook", Some(&extension));
    let made = runtime.register_function(
        "fiber.test/hook",
        command("lua-hook"),
        "hook:turn_end:0".to_owned(),
        FunctionTarget::Lua(LuaBinding {
            extension: Arc::downgrade(&extension),
            kind: LuaKind::Hook {
                point: "turn_end".to_owned(),
                index: 0,
            },
        }),
    );
    let direct = extension
        .hook_with("turn_end", 0, json!({}), |value| Ok((value, false)))
        .unwrap();
    let routed = runtime
        .hook_with(&made, json!({}), |value| Ok((value, false)))
        .unwrap();
    assert_eq!(routed.0, direct.0);
    assert_eq!(routed.1, direct.1);
    assert_eq!(routed.2, direct.2);
}

/// A Lua tool dispatched through the runtime answers exactly as a direct
/// call does.
#[test]
fn lua_tool_dispatch_matches_direct_call() {
    let home = Home::new();
    home.install(
        "tool",
        "fiber.tool(\"echo\", { description = \"echoes\", input_schema = { type = \"object\" }, effects = { effects = {}, reversible = true }, timeout = 5000, run = function(args) return { content = args.text } end })",
    );
    let session = home.load(&[]);
    let extension = session.lua_extension("fiber.test/tool").unwrap();
    let runtime = runtime();
    runtime.register_owner("fiber.test/tool", Some(&extension));
    let made = runtime.register_function(
        "fiber.test/tool",
        command("lua-tool"),
        "run".to_owned(),
        FunctionTarget::Lua(LuaBinding {
            extension: Arc::downgrade(&extension),
            kind: LuaKind::Tool {
                name: "echo".to_owned(),
            },
        }),
    );
    let cancel = support::cancel::Token::new();
    let direct = extension
        .tool_run("echo", json!({"text": "hi"}), &cancel)
        .unwrap();
    let routed = runtime
        .tool_run(&made, json!({"text": "hi"}), &cancel)
        .unwrap();
    assert_eq!(routed, direct);
}

/// A process watcher queues through its handler: a queued offer advances
/// the fan-out cursor, and the drain runs the same handler again. Each
/// phase asserts its own run, with no pinned total: the count of offers
/// the session fans out is the session's, not the runtime's, to promise.
#[test]
fn process_watcher_push_and_drain() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let runtime = runtime();
    runtime.register_owner("fiber.test/proc", None);
    let ran = Arc::new(AtomicBool::new(false));
    let counting: ProcessInvoke = {
        let ran = Arc::clone(&ran);
        Arc::new(move |_args: Value, _cancel: &dyn Cancel| {
            ran.store(true, Ordering::SeqCst);
            Ok(Some(json!({})))
        })
    };
    let watcher = runtime.register_function(
        "fiber.test/proc",
        command("c1"),
        "run".to_owned(),
        FunctionTarget::Process(counting),
    );
    assert!(!runtime.is_watcher_busy(&watcher));
    assert!(runtime.push_watcher(&watcher, json!({})).is_some());
    assert!(
        ran.load(Ordering::SeqCst),
        "the queued offer runs the watcher"
    );
    ran.store(false, Ordering::SeqCst);
    assert!(runtime.drain_watcher(&watcher, json!({})).is_ok());
    assert!(ran.load(Ordering::SeqCst), "the drain runs the watcher");
}

/// A queued process command runs on release and the waiter sees its answer:
/// the door protocol holds unchanged across backends. A failing run fails
/// the wait, for the waiter's notice.
#[test]
fn process_command_queue_runs_on_release() {
    let runtime = runtime();
    let made = process_run(&runtime, json!("done"));
    let queued = Arc::new(runtime.queue_command(&made, command("q"), "go").unwrap());
    let waiting = Arc::clone(&queued);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done_tx.send(waiting.wait());
    });
    queued.release();
    let answer = fakes::Deadline::after(Duration::from_secs(10))
        .recv(&done_rx)
        .expect("the queued command answers");
    assert_eq!(answer.unwrap(), json!("done"));
    let failed = runtime.register_function(
        "fiber.test/proc",
        command("c2"),
        "run".to_owned(),
        FunctionTarget::Process(failing("boom")),
    );
    let queued = runtime.queue_command(&failed, command("q2"), "go").unwrap();
    queued.release();
    assert!(queued.wait().is_err());
}

/// A process command that returns nothing still completes: the driver's
/// return value is ignored (`docs/extensions.md`, "Commands and screens"),
/// so there is no failure to notice. An error still fails the wait.
#[test]
fn process_command_none_answer_completes() {
    let runtime = runtime();
    runtime.register_owner("fiber.test/proc", None);
    let none = runtime.install_process_handler(
        "fiber.test/proc",
        command("c1"),
        "run",
        Arc::new(|_args: Value, _cancel: &dyn Cancel| Ok(None)),
    );
    let queued = runtime.queue_command(&none, command("q"), "go").unwrap();
    queued.release();
    assert_eq!(queued.wait().unwrap(), Value::Null);
}

/// Only its own command's hooks refuse the chain: outside any command both
/// backends pass, and a process owner never owns a command call.
#[test]
fn own_command_check_passes_outside_commands() {
    let home = Home::new();
    home.install(
        "hook",
        "fiber.hook(\"turn_end\", { phase = \"transform\", on_failure = \"non-blocking\", timeout = 5000, run = function(input) return {} end })",
    );
    let session = home.load(&[]);
    let extension = session.lua_extension("fiber.test/hook").unwrap();
    let runtime = runtime();
    runtime.register_owner("fiber.test/hook", Some(&extension));
    let lua_made = runtime.register_function(
        "fiber.test/hook",
        command("lua-hook"),
        "hook:turn_end:0".to_owned(),
        FunctionTarget::Lua(LuaBinding {
            extension: Arc::downgrade(&extension),
            kind: LuaKind::Hook {
                point: "turn_end".to_owned(),
                index: 0,
            },
        }),
    );
    assert_eq!(runtime.runs_own_command(&lua_made, "turn_end"), None);
    let proc_made = process_run(&runtime, json!(null));
    assert_eq!(runtime.runs_own_command(&proc_made, "turn_end"), None);
}

/// A runtime with no Lua owners passes a reply through untouched: there is
/// no VM to hold the request.
#[test]
fn answer_passes_through_without_lua_owners() {
    use contract::commands::ReplyAnswer;
    use contract::shapes::True;
    let runtime = runtime();
    process_run(&runtime, json!(null));
    let reply = contract::commands::Reply {
        request_id: RequestId("r1".to_owned()),
        answer: ReplyAnswer::Declined { declined: True },
    };
    let ack = contract::inbox::Ack(Box::new(|_| {}));
    let kept = runtime.answer(reply, ack);
    assert!(kept.is_some());
    let (reply, _ack) = kept.unwrap();
    assert_eq!(reply.request_id, RequestId("r1".to_owned()));
}

/// Installing a sink delivers process command failures as notices: the
/// waiter's report has somewhere to go without a VM.
#[test]
fn emit_forwards_to_installed_sink() {
    use std::sync::Mutex;
    struct Sink {
        events: Mutex<Vec<contract::events::Event>>,
    }
    impl contract::emit::Emit for Sink {
        fn emit(&self, event: &contract::events::Event) {
            self.events.lock().unwrap().push(event.clone());
        }
    }
    let runtime = runtime();
    let sink = Arc::new(Sink {
        events: Mutex::new(Vec::new()),
    });
    runtime.set_emit(sink.clone());
    runtime.emit(&contract::events::Event::Notice(contract::events::Notice {
        code: contract::ErrorCode::ExtensionFailed,
        message: "boom".to_owned(),
        extension: Some("fiber.test/proc".to_owned()),
    }));
    // The sink receives the emitted notice: the count of deliveries is the
    // emitter's, not the runtime's, to promise, so the test pins the
    // content, not the total.
    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            contract::events::Event::Notice(notice) if notice.message == "boom"
        )),
        "the sink receives the emitted notice: {events:?}"
    );
}

/// A dispatch on a dropped VM fails instead of running it: the runtime
/// keeps only a weak handle, so a dropped provider-only VM stays dropped
/// and a later `start_provider` starts fresh. Holding the VM alive here
/// would resurrect it.
#[test]
fn dispatch_on_a_dropped_vm_fails() {
    let runtime = runtime();
    let made = {
        let home = Home::new();
        home.install(
            "hook",
            "fiber.hook(\"turn_end\", { phase = \"transform\", on_failure = \"non-blocking\", timeout = 5000, run = function(input) return { seen = true } end })",
        );
        let session = home.load(&[]);
        let extension = session.lua_extension("fiber.test/hook").unwrap();
        runtime.register_owner("fiber.test/hook", Some(&extension));
        let made = runtime.register_function(
            "fiber.test/hook",
            command("lua-hook"),
            "run".to_owned(),
            FunctionTarget::Lua(LuaBinding {
                extension: Arc::downgrade(&extension),
                kind: LuaKind::Hook {
                    point: "turn_end".to_owned(),
                    index: 0,
                },
            }),
        );
        // The load holds the only other handle: dropping the session
        // drops the VM.
        drop(session);
        made
    };
    let error = runtime
        .hook_with(&made, json!({}), |value| Ok((value, false)))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("no longer running"),
        "unexpected error: {error}"
    );
    runtime.wait_for_watchers_idle("fiber.test/hook");
    assert_eq!(runtime.runs_own_command(&made, "turn_end"), None);
}

/// Provider and harness functions stay on `LuaExtension` directly: the
/// runtime carries no dispatch for them, so a provider reference is only a
/// `FunctionRef` the generation gate guards. Resolving one from a past
/// generation fails, and the current one resolves.
#[test]
fn provider_reference_resolves_only_within_its_generation() {
    let runtime = runtime();
    runtime.register_owner("fiber.test/proc", None);
    let old = runtime.install_process_handler(
        "fiber.test/proc",
        command("c1"),
        "models",
        handler(json!({"generation": 1})),
    );
    assert_eq!(old.generation, 1);
    runtime.register_owner("fiber.test/proc", None);
    let stale = runtime.resolve(&old).unwrap_err().to_string();
    assert!(stale.contains("generation 1"), "{stale}");
    let current = runtime.install_process_handler(
        "fiber.test/proc",
        command("c1"),
        "models",
        handler(json!({"generation": 2})),
    );
    assert_eq!(current.generation, 2);
    assert!(runtime.resolve(&current).is_ok());
}
