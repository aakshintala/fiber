//! `SessionExtensions::reply` across two extensions
//! (`docs/extensions.md`, "Commands and screens"): the extension holding
//! the ask takes the reply, in load order; any other reply is handed back
//! for the loop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::RequestId;
use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{InteractionRequested, ResolvedBy};
use contract::extension::ExtensionDoor;
use contract::inbox::{Ack, Delivery};
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;

use super::super::SessionExtensions;
use crate::{Origin, Request, plan};
use fakes::FakeLock;

struct Home {
    root: fakes::TempDir,
    watchdog: Option<fakes::Watchdog>,
}

impl Drop for Home {
    fn drop(&mut self) {
        self.watchdog
            .take()
            .unwrap()
            .stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    }
}

impl Home {
    fn new() -> Self {
        let watchdog = fakes::Watchdog::matching("fiber-door-reply");
        let root = fakes::TempDir::new("fiber-door-reply");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self {
            root,
            watchdog: Some(watchdog),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1})
                .to_string(),
        )
        .unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    #[track_caller]
    fn load(&self) -> Arc<SessionExtensions> {
        self.load_clock(FakeClock::new())
    }

    fn load_clock(&self, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
        self.load_settings(clock, &[])
    }

    fn load_settings(&self, clock: Arc<FakeClock>, overrides: &[&str]) -> Arc<SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        Arc::new(bounded(move || {
            SessionExtensions::load(&home, &config, clock, locks, None)
        }))
    }
}

fn asking(short: &str) -> String {
    format!(
        "fiber.command(\"go\", {{ timeout = 5000, run = function()\n\
         local answer = host.ask(\"confirm\", {{ prompt = \"{short}?\" }})\n\
         return tostring(answer.confirmed) end }})\n"
    )
}

/// Runs `command` on `session`'s `which`-th extension and sends its result.
fn run(
    session: &Arc<SessionExtensions>,
    which: usize,
    command: &str,
) -> mpsc::Receiver<Result<String, crate::Error>> {
    let lua = Arc::clone(&session.lua[which]);
    let command = command.to_owned();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = done_tx.send(lua.command(&command, ""));
    });
    done_rx
}

#[track_caller]
fn interaction(rx: &mpsc::Receiver<Delivery>, wait: &Deadline) -> InteractionRequested {
    let Delivery::Interaction(requested) = wait.recv(rx).expect("the Interaction arrives") else {
        panic!("an unexpected delivery arrives");
    };
    requested
}

/// Runs `f` on its own thread and waits for it under [`MUST_SUCCEED_WITHIN`], so a load
/// that never returns fails the test instead of hanging it.
#[track_caller]
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the extensions")
}

#[test]
fn the_extension_holding_the_ask_takes_the_reply_in_load_order() {
    let home = Home::new();
    home.install("a", &asking("a"));
    home.install("b", &asking("b"));
    let session = home.load();
    let (inbox_tx, rx) = mpsc::channel();
    for lua in &session.lua {
        lua.deliver_to(inbox_tx.clone());
    }
    session.answerable(true);
    // Both commands park on their asks; each Interaction names its own
    // extension, whatever order they arrive in.
    let first = run(&session, 0, "go");
    let second = run(&session, 1, "go");
    let mut ids = std::collections::BTreeMap::new();
    // One deadline for the whole wait: both Interactions arrive on one
    // stream with nothing between them but a map insert.
    let wait = Deadline::after(MUST_SUCCEED_WITHIN);
    for _ in 0..2 {
        let requested = interaction(&rx, &wait);
        ids.insert(
            requested.extension.clone().unwrap_or_default(),
            requested.request_id,
        );
    }
    let (id_a, id_b) = (
        ids.remove("fiber.test/a").expect("a asks"),
        ids.remove("fiber.test/b").expect("b asks"),
    );
    // A reply for B passes through A (which hands it back) to B.
    let (tx, accepted) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer.is_ok());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_b.clone(),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_none(),
        "the holding extension takes the reply"
    );
    let Delivery::Resolved(resolved, ack) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    assert_eq!(resolved.request_id, id_b);
    assert_eq!(resolved.by, ResolvedBy::Person);
    (ack.0)(Ok(None));
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&accepted)
            .expect("the reply is answered"),
        "the reply is accepted once the line is in the log"
    );
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&second)
            .expect("the command returns")
            .unwrap(),
        "true"
    );
    // A's ask is still pending: a fitting reply resolves it too.
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: id_a,
                    answer: ReplyAnswer::Confirmed { confirmed: false },
                },
                ack
            )
            .is_none()
    );
    let Delivery::Resolved(_, ack) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the Resolved arrives")
    else {
        panic!("a Resolved arrives");
    };
    (ack.0)(Ok(None));
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&first)
            .expect("the command returns")
            .unwrap(),
        "false"
    );
}

fn tool_answer() -> Box<contract::events::ToolCallCompleted> {
    Box::new(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        content: vec![contract::shapes::ContentPart::Text {
            text: "answer".into(),
        }],
        reason: None,
        error: None,
        process: None,
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

#[test]
fn an_admitted_command_holds_its_runner_through_success_and_lua_error() {
    for failing in [false, true] {
        let home = Home::new();
        home.install("a", &format!("fiber.command('go', {{ timeout = 5000, run = function() local o = host.tool('read', {{path='x'}}) {} return o.status .. '|' .. o.content[1].text end }})", if failing { "error('boom')" } else { "" }));
        let session = home.load();
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("c_admitted".into()), "go", "")
            .unwrap();
        assert_eq!(session.lua[0].runners_held(), 1);
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "admitted command inner call")
        else {
            panic!("expected inner call");
        };
        assert_eq!(request.command.0, "c_admitted");
        assert_eq!(request.extension, "fiber.test/a");
        assert_eq!(request.name, "read");
        assert_eq!(request.arguments, json!({"path":"x"}));
        assert!(!request.cancel.is_cancelled());
        (request.taken)();
        (request.answer)(tool_answer());
        let returned =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&result, "admitted command end");
        if failing {
            assert!(returned.unwrap_err().to_string().contains("boom"));
        } else {
            assert_eq!(returned.unwrap(), json!("completed|answer"));
        }
        assert_eq!(session.lua[0].runners_held(), 0);
        assert!(request.cancel.is_cancelled());
    }
}

#[test]
fn a_command_deadline_ends_its_runner_at_but_not_below_the_boundary() {
    for milliseconds in [99, 100] {
        let home = Home::new();
        home.install("a", "fiber.command('go', { timeout = 100, run = function() return host.tool('read', {}).status end })");
        let clock = FakeClock::new();
        let session = home.load_clock(clock.clone());
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("c_timeout".into()), "go", "")
            .unwrap();
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        let caller = thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "parked command inner call")
        else {
            panic!("expected inner call");
        };
        (request.taken)();
        let until = clock.origin() + Duration::from_millis(1100);
        assert!(clock.await_thread_parked(
            caller.thread().id(),
            Some(until),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ));
        // The VM ends a parked callback at its own deadline; the caller's
        // grace is a backstop, not extra command time.
        let parked = clock
            .mark_parked(
                clock.origin() + Duration::from_millis(100),
                fakes::Deadline::after(MUST_SUCCEED_WITHIN),
            )
            .expect("the scheduler parks at the command deadline");
        let parked = parked.threads();
        assert_eq!(
            parked.len(),
            1,
            "only the scheduler parks at the command deadline"
        );
        let vm = parked.into_iter().next().expect("the scheduler thread");
        let park = clock.origin() + Duration::from_millis(100);
        clock.advance_when_parked(
            Some(park),
            Some(vm),
            Duration::from_millis(milliseconds),
            &Deadline::after(MUST_SUCCEED_WITHIN),
        );
        if milliseconds < 100 {
            assert!(!request.cancel.is_cancelled());
            assert_eq!(session.lua[0].runners_held(), 1);
            assert!(result.try_recv().is_err());
            assert!(clock.await_thread_parked(
                caller.thread().id(),
                Some(until),
                fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            ));
            clock.advance_when_parked(
                Some(park),
                Some(vm),
                Duration::from_millis(1),
                &Deadline::after(MUST_SUCCEED_WITHIN),
            );
        }
        assert!(matches!(
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&result, "command timeout"),
            Err(crate::Error::Timeout { .. })
        ));
        assert_eq!(session.lua[0].runners_held(), 0);
        assert!(request.cancel.is_cancelled());
        (request.answer)(tool_answer());
        assert_eq!(session.lua[0].runners_held(), 0);
    }
}

fn before_call(session: &SessionExtensions) -> contract::hook::BeforeToolAnswer {
    use contract::hook::Hooks;
    session.before_tool(&contract::hook::BeforeToolCall {
        tool: "read",
        arguments: &serde_json::Map::new(),
        declared: &contract::shapes::DeclaredEffects {
            effects: vec![contract::shapes::Effect::Reads],
            reversible: true,
            paths: None,
        },
    })
}

#[test]
fn an_untaken_request_keeps_stream_order_under_both_timeout_orders() {
    use contract::hook::BeforeToolOutcome;
    for command_first in [false, true] {
        let home = Home::new();
        let marker = home.root.path().join("workspace/hook-ran");
        let (command_ms, hook_ms) = if command_first {
            (1000, 5000)
        } else {
            (5000, 1000)
        };
        home.install("x", &format!("fiber.command('go', {{timeout={command_ms}, run=function() return host.tool('read', {{}}).status end}})\nfiber.hook('before_tool', {{timeout={hook_ms}, on_failure='blocking', run=function() host.fs.write({marker:?}, 'x') end}})"));
        let clock = FakeClock::new();
        let session = home.load_clock(clock.clone());
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("c_wait".into()), "go", "")
            .unwrap();
        let queued = admitted.queued().clone();
        queued.release();
        let (send, command_result) = mpsc::channel();
        let command_thread = thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "untaken command request")
        else {
            panic!("expected inner call");
        };
        assert!(!session.lua[0].runs_own_command_call());
        let caller = session.clone();
        let (send, hook_result) = mpsc::channel();
        let hook_thread = thread::spawn(move || {
            let _sent = send.send(before_call(&caller));
        });
        assert!(clock.await_thread_parked(
            command_thread.thread().id(),
            Some(clock.origin() + Duration::from_millis(command_ms + 1000)),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ));
        assert!(clock.await_thread_parked(
            hook_thread.thread().id(),
            Some(clock.origin() + Duration::from_millis(hook_ms)),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ));
        assert!(!marker.exists());
        let (park, thread) = if command_first {
            (
                clock.origin() + Duration::from_millis(2000),
                command_thread.thread().id(),
            )
        } else {
            (
                clock.origin() + Duration::from_millis(1000),
                hook_thread.thread().id(),
            )
        };
        clock.advance_when_parked(
            Some(park),
            Some(thread),
            park - clock.now(),
            &Deadline::after(MUST_SUCCEED_WITHIN),
        );
        if command_first {
            assert!(matches!(
                Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv_or_fail(&command_result, "command timeout before hook"),
                Err(crate::Error::Timeout { .. })
            ));
            assert!(request.cancel.is_cancelled());
            let answer = Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&hook_result, "hook after command timeout");
            assert!(matches!(answer.outcome, BeforeToolOutcome::Unchanged));
            assert!(answer.notices.is_empty());
            assert_eq!(fs::read_to_string(&marker).unwrap(), "x");
            (request.taken)();
            assert!(!session.lua[0].runs_own_command_call());
        } else {
            let answer = Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&hook_result, "hook timeout before command");
            let BeforeToolOutcome::Failed(failure) = answer.outcome else {
                panic!("expected hook timeout");
            };
            assert_eq!(failure.extension, "fiber.test/x");
            assert!(failure.message.contains("1000"));
            assert!(!failure.message.contains("wait behind"));
            assert!(!marker.exists());
            assert!(!request.cancel.is_cancelled());
            assert!(command_result.try_recv().is_err());
            (request.answer)(tool_answer());
            assert_eq!(
                Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv_or_fail(&command_result, "command after hook timeout")
                    .unwrap(),
                json!("completed")
            );
        }
        assert_eq!(session.lua[0].runners_held(), 0);
    }
}

#[test]
fn the_commands_own_tool_runs_in_a_gap_without_tool_hooks() {
    let home = Home::new();
    home.install("x", "fiber.command('go', {timeout=5000, run=function() return host.tool('local_probe', {}).content[1].text end})\nfiber.tool('local_probe', {description='d', input_schema={type='object'}, effects={effects={'reads'}, reversible=true}, timeout=5000, run=function() return 'gap' end})");
    let session = home.load();
    let (send, rx) = mpsc::channel();
    session.lua[0].deliver_to(send);
    let admitted = session
        .commands
        .admit_with(&contract::CommandId("c_gap".into()), "go", "")
        .unwrap();
    let queued = admitted.queued().clone();
    queued.release();
    let (send, result) = mpsc::channel();
    thread::spawn(move || {
        let _sent = send.send(queued.wait());
    });
    let Delivery::InnerCall(request) =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "own tool command request")
    else {
        panic!("expected inner call");
    };
    assert_eq!(request.name, "local_probe");
    (request.taken)();
    let lua = session.lua[0].clone();
    assert_eq!(
        bounded(move || lua.tool_run("local_probe", json!({}), &fakes::CancelToken::new()))
            .unwrap(),
        Some(json!("gap"))
    );
    let mut answer = tool_answer();
    answer.content = vec![contract::shapes::ContentPart::Text { text: "gap".into() }];
    (request.answer)(answer);
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&result, "own tool command completion")
            .unwrap(),
        json!("gap")
    );
}

#[test]
fn own_tool_hooks_fail_when_reached_and_run_normally_after_the_answer() {
    use contract::hook::{AfterToolCall, AfterToolOutcome, BeforeToolOutcome, CheckOutcome, Hooks};
    for (point, phase, policy) in [
        ("before_tool", "transform", "blocking"),
        ("before_tool", "transform", "non-blocking"),
        ("before_tool", "sanitize", "blocking"),
        ("before_tool", "check", "blocking"),
        ("after_tool", "sanitize", "blocking"),
    ] {
        let home = Home::new();
        let marker = home.root.path().join("workspace/own-hook");
        home.install("x", &format!("fiber.command('go', {{timeout=5000, run=function() return host.tool('read', {{}}).status end}})\nfiber.hook('{point}', {{phase='{phase}', timeout=1000, on_failure='{policy}', run=function() host.fs.write({marker:?}, 'x') end}})"));
        let session = home.load();
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("c_own".into()), "go", "")
            .unwrap();
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "own-hook request")
        else {
            panic!("expected inner call");
        };
        assert!(!session.lua[0].runs_own_command_call());
        (request.taken)();
        assert!(session.lua[0].runs_own_command_call());
        let caller = session.clone();
        let failure = bounded(move || {
            if point == "after_tool" {
                let answer = caller.after_tool(&AfterToolCall {
                    tool: "read",
                    arguments: &serde_json::Map::new(),
                    status: contract::events::CallStatus::Completed,
                    content: "raw",
                    details: None,
                    process: None,
                    spilled: None,

                    not_regular: false,
                    delivery: None,
                });
                assert!(answer.notices.is_empty());
                let AfterToolOutcome::Failed(failure) = answer.outcome else {
                    panic!("own after hook must fail");
                };
                failure
            } else if phase == "check" {
                let answer = caller.check_tool(&contract::hook::BeforeToolCall {
                    tool: "read",
                    arguments: &serde_json::Map::new(),
                    declared: &contract::shapes::DeclaredEffects {
                        effects: vec![contract::shapes::Effect::Reads],
                        reversible: true,
                        paths: None,
                    },
                });
                assert!(answer.notices.is_empty());
                let CheckOutcome::Failed(failure) = answer.outcome else {
                    panic!("own check hook must fail");
                };
                failure
            } else {
                let answer = before_call(&caller);
                assert!(answer.notices.is_empty());
                let BeforeToolOutcome::Failed(failure) = answer.outcome else {
                    panic!("own before hook must fail even when non-blocking");
                };
                failure
            }
        });
        assert_eq!(failure.extension, "fiber.test/x");
        assert!(failure.message.contains("wait behind"));
        assert!(!marker.exists());
        (request.answer)(tool_answer());
        assert!(!session.lua[0].runs_own_command_call());
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&result, "command before normal hook")
            .unwrap();
        let caller = session.clone();
        bounded(move || {
            if point == "after_tool" {
                assert!(matches!(
                    caller
                        .after_tool(&AfterToolCall {
                            tool: "read",
                            arguments: &serde_json::Map::new(),
                            status: contract::events::CallStatus::Completed,
                            content: "raw",
                            details: None,
                            process: None,
                            spilled: None,
                            not_regular: false,
                            delivery: None,
                        })
                        .outcome,
                    AfterToolOutcome::Unchanged
                ));
            } else if phase == "check" {
                assert!(matches!(
                    caller
                        .check_tool(&contract::hook::BeforeToolCall {
                            tool: "read",
                            arguments: &serde_json::Map::new(),
                            declared: &contract::shapes::DeclaredEffects {
                                effects: vec![contract::shapes::Effect::Reads],
                                reversible: true,
                                paths: None
                            }
                        })
                        .outcome,
                    CheckOutcome::Passed
                ));
            } else {
                assert!(matches!(
                    before_call(&caller).outcome,
                    BeforeToolOutcome::Unchanged
                ));
            }
        });
        assert_eq!(fs::read_to_string(marker).unwrap(), "x");
    }
}

#[test]
fn earlier_hooks_run_and_a_refusal_before_the_own_hook_wins() {
    use contract::hook::BeforeToolOutcome;
    for refusing in [true, false] {
        let home = Home::new();
        let own = home.root.path().join("workspace/x");
        let earlier = home.root.path().join("workspace/y");
        home.install("x", &format!("fiber.command('go', {{timeout=5000,run=function() return host.tool('read',{{}}).status end}})\nfiber.hook('before_tool', {{timeout=1000,on_failure='blocking',run=function() host.fs.write({own:?},'x') end}})"));
        home.install("y", &format!("fiber.hook('before_tool', {{timeout=1000,on_failure='blocking',run=function() host.fs.write({earlier:?},'y') {} end}})", if refusing {"return {refuse='earlier deny'}"} else {""}));
        let session = home.load_settings(
            FakeClock::new(),
            &["hooks.order.before_tool=[\"fiber.test/y\",\"fiber.test/x\"]"],
        );
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("c_order".into()), "go", "")
            .unwrap();
        let queued = admitted.queued().clone();
        queued.release();
        let (send, result) = mpsc::channel();
        thread::spawn(move || {
            let _sent = send.send(queued.wait());
        });
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "ordered-hook request")
        else {
            panic!("expected inner call");
        };
        (request.taken)();
        let caller = session.clone();
        let answer = bounded(move || before_call(&caller));
        assert!(answer.notices.is_empty());
        if refusing {
            let BeforeToolOutcome::Refused(refusal) = answer.outcome else {
                panic!("earlier refusal must win");
            };
            assert_eq!(refusal.extension, "fiber.test/y");
            assert_eq!(refusal.reason, "earlier deny");
        } else {
            let BeforeToolOutcome::Failed(failure) = answer.outcome else {
                panic!("own hook must fail after earlier hook");
            };
            assert_eq!(failure.extension, "fiber.test/x");
        }
        assert_eq!(fs::read_to_string(earlier).unwrap(), "y");
        assert!(!own.exists());
        (request.answer)(tool_answer());
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv_or_fail(&result, "ordered command end")
            .unwrap();
    }
}

struct Events(mpsc::Sender<contract::events::Event>);
impl contract::emit::Emit for Events {
    fn emit(&self, event: &contract::events::Event) {
        let _sent = self.0.send(event.clone());
    }
}

#[test]
fn the_door_returns_a_tool_failure_normally_and_ends_a_running_call_at_timeout() {
    struct EndNotice(mpsc::Sender<()>);
    impl contract::clock::Wake for EndNotice {
        fn wake(&self) {
            let _sent = self.0.send(());
        }
    }
    for ending in ["completed", "failed", "timeout"] {
        let home = Home::new();
        home.install("x", "fiber.command('go', {timeout=1000,run=function() local o=host.tool('spin', {}) host.status(o.status .. ':' .. (o.error and o.error.code or o.content[1].text)) end})");
        let clock = FakeClock::new();
        let session = home.load_clock(clock.clone());
        let (send, rx) = mpsc::channel();
        session.lua[0].deliver_to(send);
        let (send, events) = mpsc::channel();
        session.emit_to(Arc::new(Events(send)));
        session
            .command(&contract::CommandId("c_door".into()), "go", "")
            .unwrap()();
        let Delivery::InnerCall(request) =
            Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "door command request")
        else {
            panic!("expected inner call");
        };
        assert_eq!(request.command.0, "c_door");
        assert_eq!(session.lua[0].runners_held(), 1);
        let (send, ended) = mpsc::channel();
        let wake: Arc<dyn contract::clock::Wake> = Arc::new(EndNotice(send));
        request.cancel.subscribe(Arc::downgrade(&wake));
        (request.taken)();
        assert!(session.lua[0].runs_own_command_call());
        let parked = clock
            .mark_parked(
                clock.origin() + Duration::from_secs(2),
                fakes::Deadline::after(MUST_SUCCEED_WITHIN),
            )
            .expect("the call parks at its timeout plus the grace");
        let parked = parked.threads();
        assert_eq!(
            parked.len(),
            1,
            "one thread parks at the timeout plus the grace"
        );
        let parked = parked.into_iter().next().expect("the parked thread");
        if ending == "timeout" {
            clock.advance_when_parked(
                Some(clock.origin() + Duration::from_secs(2)),
                Some(parked),
                Duration::from_millis(2000),
                &Deadline::after(MUST_SUCCEED_WITHIN),
            );
            let notice = Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&events, "command timeout notice");
            let contract::events::Event::Notice(notice) = notice else {
                panic!("expected timeout notice");
            };
            assert_eq!(notice.code, contract::ErrorCode::ExtensionFailed);
            assert!(notice.message.contains("go"));
            assert!(notice.message.contains("1000"));
            assert!(!session.lua[0].runs_own_command_call());
            assert!(request.cancel.is_cancelled());
            (request.answer)(tool_answer());
        } else {
            let mut answer = tool_answer();
            if ending == "failed" {
                answer.status = contract::events::CallStatus::Failed;
                answer.error = Some(contract::shapes::Failure {
                    code: contract::ErrorCode::HookFailed,
                    message: "own hook".into(),
                    provider: None,
                    retry_after_ms: None,
                });
            }
            (request.answer)(answer);
            assert!(!session.lua[0].runs_own_command_call());
            let status = Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&events, "ordinary tool outcome status");
            let contract::events::Event::ExtensionUi(ui) = status else {
                panic!("expected ordinary outcome status");
            };
            let contract::events::Ui::Status { status } = ui.ui else {
                panic!("expected status UI");
            };
            assert_eq!(
                status,
                if ending == "failed" {
                    "failed:hook_failed"
                } else {
                    "completed:answer"
                }
            );
        }
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&ended, "command cleanup wake");
        assert_eq!(session.lua[0].runners_held(), 0);
        assert!(request.cancel.is_cancelled());
        assert!(events.try_recv().is_err());
    }
}

#[test]
fn a_reply_no_extension_holds_is_handed_back() {
    let home = Home::new();
    home.install("a", &asking("a"));
    let session = home.load();
    session.answerable(true);
    let (tx, _) = mpsc::channel();
    let ack = Ack(Box::new(move |_| {
        let _sent = tx.send(());
    }));
    assert!(
        session
            .reply(
                Reply {
                    request_id: RequestId("r_other".into()),
                    answer: ReplyAnswer::Confirmed { confirmed: true },
                },
                ack
            )
            .is_some(),
        "an unknown id is handed back for the loop's stale_request"
    );
}
