use std::sync::Arc;

use contract::events::{CallStatus, Event, HandoffTrigger, OnFork};
use contract::hook::*;
use serde_json::{Map, json};

use super::SessionExtensions;
use crate::test_support::{Home, bounded};

#[test]
fn state_after_tool_each_changed_field_carries_its_writes() {
    for returned in [
        "{content='text'}",
        "{details={n=1}}",
        "{artifact='artifact'}",
    ] {
        let home = Home::new();
        home.install(
            "goal",
            &hook(
                "after_tool",
                &format!("state.set('k',1); return {returned}"),
                "blocking",
            ),
        );
        let session = home.load(&[]);
        run(&session, "after_tool");
        assert_eq!(session.take_change_state().len(), 1, "{returned}");
        assert!(session.take_state_lines().is_empty());
    }
}

#[test]
fn state_multiple_extensions_keep_chain_order_and_context_indices() {
    for point in [
        "after_tool",
        "before_message",
        "session_start",
        "turn_start",
        "turn_end",
    ] {
        let home = Home::new();
        let returned = match point {
            "after_tool" => "{content='text'}",
            "turn_end" => "{message='context'}",
            _ => "{context='context'}",
        };
        for name in ["b", "a"] {
            home.install(
                name,
                &hook(
                    point,
                    &format!("state.set('k',1); return {returned}"),
                    "blocking",
                ),
            );
        }
        let session = home.load(&[]);
        run(&session, point);
        let lines = session.take_change_state();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.extension.as_str())
                .collect::<Vec<_>>(),
            ["fiber.test/a", "fiber.test/b"]
        );
        assert_eq!(
            lines.iter().map(|line| line.context).collect::<Vec<_>>(),
            if point == "after_tool" {
                vec![None, None]
            } else {
                vec![Some(0), Some(1)]
            }
        );
        assert!(session.take_state_lines().is_empty());
    }
    let home = Home::new();
    home.install("empty", "");
    let session = home.load(&[]);
    assert!(session.has_extensions());
    assert!(!session.has_hooks());
    assert!(session.take_state_lines().is_empty());
}

#[test]
fn state_without_extensions_reports_none_loaded() {
    let home = Home::new();
    let session = home.load(&[]);
    assert!(!session.has_extensions());
}

#[test]
fn state_closed_session_refuses_later_writes() {
    let home = Home::new();
    home.install("goal", "");
    let session = home.load(&[]);
    session.close_state();
    let store = session.state.store("fiber.test/a");
    assert!(matches!(
        store.set(
            crate::host::state::Scope::Other,
            "k",
            json!(1),
            OnFork::AtPoint
        ),
        Err(crate::host::state::StateError::Closing)
    ));
}

#[test]
fn state_late_provider_vm_uses_the_sessions_live_registry() {
    for hooked in [false, true] {
        let home = Home::new();
        let mut init = "state.set('starts',(state.get('starts') or 0)+1);\n".to_owned();
        if hooked {
            init += &hook("after_tool", "state.set('starts',10)", "blocking");
        }
        home.install("goal", &init);
        let package = home.home().join("extensions/fiber.test-goal");
        let manifest = package.join("extension.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        value["providers"] = json!({"p":[]});
        std::fs::write(manifest, value.to_string()).unwrap();
        std::fs::create_dir(package.join("providers")).unwrap();
        std::fs::write(package.join("providers/p.lua"), "fiber.provider('p',{models={timeout=5000,run=function() state.set('provider',state.get('starts')); return {} end}})").unwrap();
        let mut session = home.load(&[]);
        assert!(session.notices().is_empty(), "{:?}", session.notices());
        if hooked {
            run(&session, "after_tool");
        }
        Arc::get_mut(&mut session)
            .unwrap()
            .retain_lua_providers(&[]);
        let config = config::Config::load(config::Sources {
            home: home.home(),
            workspace: home.home().parent().unwrap().join("workspace"),
            project: config::ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let held = session.clone();
        bounded(move || {
            let provider = held
                .start_provider(
                    &config,
                    fakes::clock::FakeClock::new(),
                    Arc::new(fakes::FakeLock::new()),
                    "fiber.test/goal",
                    "p",
                )
                .unwrap();
            assert!(provider.registers("models").unwrap());
            assert!(provider.models().unwrap().is_empty());
        });
        let expected = if hooked { 10 } else { 2 };
        let store = session.state.store("fiber.test/goal");
        assert_eq!(store.get("starts"), Some(json!(expected)));
        assert_eq!(store.get("provider"), Some(json!(expected)));
    }
}

#[test]
fn state_own_command_failure_drops_earlier_changes_not_other_writes() {
    for (point, returned) in [
        ("before_tool", "{arguments={n=1}}"),
        ("after_tool", "{content='changed'}"),
    ] {
        let home = Home::new();
        home.install(
            "a",
            &hook(
                point,
                &format!("state.set('k',1); return {returned}"),
                "blocking",
            ),
        );
        home.install("b", &(hook(point, "state.set('own',true)", "blocking") + "fiber.command('go',{timeout=5000,run=function() return host.tool('read',{}).status end})"));
        let session = home.load(&[]);
        let (tx, rx) = std::sync::mpsc::channel();
        session.deliver_to(tx);
        let admitted = session
            .commands
            .admit_with(&contract::CommandId("state-own".into()), "go", "")
            .unwrap();
        let queued = admitted.queued().clone();
        let (tx, done) = std::sync::mpsc::channel();
        queued.release();
        std::thread::spawn(move || {
            let _sent = tx.send(queued.wait());
        });
        let deadline = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        let contract::inbox::Delivery::InnerCall(request) =
            deadline.recv_or_fail(&rx, "own command suspended on its inner call")
        else {
            panic!("inner call")
        };
        (request.taken)();
        run(&session, point);
        assert!(session.take_change_state().is_empty());
        assert!(session.take_state_lines().is_empty());
        assert_eq!(session.state.store("fiber.test/a").get("k"), Some(json!(1)));
        assert_eq!(session.state.store("fiber.test/b").get("own"), None);
        session
            .state
            .store("fiber.test/a")
            .set(
                crate::host::state::Scope::Other,
                "timer",
                json!(true),
                OnFork::AtPoint,
            )
            .unwrap();
        assert_eq!(session.take_state_lines().len(), 1);
        (request.answer)(Box::new(contract::events::ToolCallCompleted {
            status: CallStatus::Completed,
            content: Vec::new(),
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
        }));
        assert_eq!(
            deadline
                .recv_or_fail(&done, "own command completed")
                .unwrap(),
            json!("completed")
        );
    }
}

fn dispatch(session: &SessionExtensions, point: &str) {
    let cancel = fakes::CancelToken::new();
    let arguments = Map::new();
    let call = BeforeToolCall {
        tool: "read",
        arguments: &arguments,
        declared: &contract::shapes::DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
    };
    match point {
        "after_tool" => {
            session.after_tool(&AfterToolCall {
                tool: "read",
                arguments: &arguments,
                status: CallStatus::Completed,
                content: "original",
                details: None,
                process: None,
                spilled: None,
            });
        }
        "before_tool" => {
            session.before_tool(&call);
        }
        "check_tool" => {
            session.check_tool(&call);
        }
        "before_message" => {
            session.before_message(&BeforeMessage { content: &[] });
        }
        "session_start" => {
            session.session_start(&SessionStart {
                reason: StartReason::Resume,
                session_id: &contract::SessionId("s".into()),
                workspace: "/work",
                cancel: &cancel,
            });
        }
        "turn_start" => {
            session.turn_start(&TurnStart {
                input: &[],
                cancel: &cancel,
            });
        }
        "turn_end" => {
            session.turn_end(&TurnEnd {
                reply: "reply",
                cancel: &cancel,
            });
        }
        "before_model_call" => {
            session.before_model_call(&BeforeModelCall {
                model: "test/model",
                usage: &Default::default(),
                cancel: &cancel,
            });
        }
        "before_handoff" => {
            session.before_handoff(&BeforeHandoff {
                trigger: HandoffTrigger::Person,
                instructions: None,
                conversation: &[],
                cancel: &cancel,
            });
        }
        _ => panic!("unknown point"),
    }
}

fn run(session: &Arc<SessionExtensions>, point: &'static str) {
    let session = session.clone();
    bounded(move || dispatch(&session, point));
}

fn hook(point: &str, body: &str, blocking: &str) -> String {
    let phase = if point == "check_tool" {
        ",phase='check'"
    } else {
        ""
    };
    let point = if point == "check_tool" {
        "before_tool"
    } else {
        point
    };
    format!(
        "fiber.hook('{point}', {{timeout=5000,on_failure='{blocking}'{phase},run=function(arg) {body} end}})\n"
    )
}

#[test]
fn state_seed_reaches_session_start_before_the_first_call() {
    let home = Home::new();
    home.install(
        "goal",
        &hook(
            "session_start",
            "state.set('seen',state.get('goal')); return {context='start'}",
            "blocking",
        ),
    );
    let seed = StateSeed::from([(
        "fiber.test/goal".into(),
        [("goal".into(), (json!({"n":3}), OnFork::AtPoint))].into(),
    )]);
    let session = home.load_seeded(&[], seed);
    assert!(session.take_state_lines().is_empty());
    run(&session, "session_start");
    let lines = session.take_change_state();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].context, Some(0));
    let Event::ExtensionStateSet(set) = lines[0].clone().into_event() else {
        panic!("set")
    };
    assert_eq!(set.key, "seen");
    assert_eq!(set.value, json!({"n":3}));
    assert!(session.take_state_lines().is_empty());
}

#[test]
fn state_changed_hooks_carry_and_no_change_hooks_drain_once() {
    for (point, returned) in [
        ("after_tool", "{content='changed'}"),
        ("before_tool", "{arguments={x=1}}"),
        ("before_message", "{context='context'}"),
        ("session_start", "{context='context'}"),
        ("turn_start", "{context='context'}"),
        ("turn_end", "{message='message'}"),
        ("before_handoff", "{note='note'}"),
    ] {
        for (return_value, changed) in [(returned, true), ("nil", false)] {
            let home = Home::new();
            home.install(
                "goal",
                &hook(
                    point,
                    &format!("state.set('k',1); return {return_value}"),
                    "blocking",
                ),
            );
            let session = home.load(&[]);
            run(&session, point);
            assert_eq!(
                session.state.store("fiber.test/goal").get("k"),
                Some(json!(1))
            );
            let lines = session.take_change_state();
            assert_eq!(lines.len(), usize::from(changed), "{point}");
            assert_eq!(
                session.take_state_lines().len(),
                usize::from(!changed),
                "{point}"
            );
            assert!(session.take_state_lines().is_empty());
            assert!(session.take_change_state().is_empty());
        }
    }
}

#[test]
fn state_failed_calls_discard_their_writes_including_bad_returns() {
    for policy in ["blocking", "non-blocking"] {
        for returned in ["error('boom')", "return {status='invalid'}"] {
            let home = Home::new();
            home.install(
                "goal",
                &hook(
                    "after_tool",
                    &format!("state.set('k',1); {returned}"),
                    policy,
                ),
            );
            let seed = StateSeed::from([(
                "fiber.test/goal".into(),
                [("k".into(), (json!(0), OnFork::Latest))].into(),
            )]);
            let session = home.load_seeded(&[], seed);
            run(&session, "after_tool");
            assert!(session.take_change_state().is_empty());
            assert!(session.take_state_lines().is_empty());
            assert_eq!(
                session.state.store("fiber.test/goal").get("k"),
                Some(json!(0))
            );
        }
    }
}

#[test]
fn state_later_chain_failures_drop_carried_lines_but_keep_the_taken_view() {
    for (point, returned) in [
        ("after_tool", "{content='changed'}"),
        ("before_tool", "{arguments={x=1}}"),
        ("before_message", "{context='context'}"),
        ("session_start", "{context='context'}"),
        ("turn_start", "{context='context'}"),
        ("turn_end", "{message='message'}"),
        ("before_handoff", "{note='note'}"),
    ] {
        for policy in ["blocking", "non-blocking"] {
            let home = Home::new();
            let init = hook(
                point,
                &format!("state.set('k',1); return {returned}"),
                "blocking",
            ) + &hook(point, "state.set('k',2); error('later failure')", policy);
            home.install("goal", &init);
            let session = home.load(&[]);
            run(&session, point);
            assert_eq!(
                session.take_change_state().len(),
                usize::from(policy == "non-blocking"),
                "{point}/{policy}"
            );
            assert!(session.take_state_lines().is_empty());
            assert_eq!(
                session.state.store("fiber.test/goal").get("k"),
                Some(json!(1)),
                "{point}"
            );
        }
    }
}

#[test]
fn state_later_refusals_drop_changes_but_not_unchanged_hook_lines() {
    for (point, returned) in [
        ("before_tool", "{arguments={x=1}}"),
        ("before_message", "{context='context'}"),
        ("check_tool", "nil"),
        ("before_model_call", "nil"),
    ] {
        let home = Home::new();
        home.install(
            "goal",
            &(hook(
                point,
                &format!("state.set('k',1); return {returned}"),
                "blocking",
            ) + &hook(
                point,
                "state.set('refusal',true); return {refuse='no'}",
                "blocking",
            )),
        );
        let session = home.load(&[]);
        run(&session, point);
        assert!(session.take_change_state().is_empty());
        let events = session.take_state_lines();
        let expected = if matches!(point, "check_tool" | "before_model_call") {
            2
        } else {
            1
        };
        assert_eq!(events.len(), expected, "{point}");
        assert_eq!(
            session.state.store("fiber.test/goal").get("k"),
            Some(json!(1))
        );
    }
}

#[test]
fn state_superseded_changes_keep_both_hooks_lines_in_order() {
    for (point, first, second) in [
        (
            "before_tool",
            "return {arguments={n=1}}",
            "assert(arg.arguments.n == 1); return {arguments={n=2}}",
        ),
        (
            "before_handoff",
            "return {note='first'}",
            "assert(arg.note == 'first'); return {note='second'}",
        ),
    ] {
        let home = Home::new();
        home.install(
            "goal",
            &(hook(point, &format!("state.set('k',1); {first}"), "blocking")
                + &hook(point, &format!("state.set('k',2); {second}"), "blocking")),
        );
        let session = home.load(&[]);
        run(&session, point);
        let lines = session.take_change_state();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines.iter().map(|line| line.landing).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(session.admit_state_lines(lines).len(), 2);
        assert_eq!(
            session.state.store("fiber.test/goal").get("k"),
            Some(json!(2))
        );
        assert!(session.take_state_lines().is_empty());
    }
}

#[test]
fn state_admission_drains_other_writes_before_carried_lines() {
    for later in [false, true] {
        let home = Home::new();
        home.install(
            "goal",
            &hook(
                "after_tool",
                "state.set('k','hook'); return {content='changed'}",
                "blocking",
            ),
        );
        let session = home.load(&[]);
        let store = session.state.store("fiber.test/goal");
        store
            .set(
                crate::host::state::Scope::Other,
                "k",
                json!("timer-before"),
                OnFork::AtPoint,
            )
            .unwrap();
        run(&session, "after_tool");
        let lines = session.take_change_state();
        if later {
            store
                .set(
                    crate::host::state::Scope::Other,
                    "k",
                    json!("timer-after"),
                    OnFork::AtPoint,
                )
                .unwrap();
        }
        let events = session.admit_state_lines(lines);
        let values: Vec<_> = events
            .into_iter()
            .map(|event| {
                let Event::ExtensionStateSet(set) = event else {
                    panic!("set")
                };
                set.value
            })
            .collect();
        assert_eq!(
            values,
            if later {
                vec![json!("timer-before"), json!("timer-after")]
            } else {
                vec![json!("timer-before"), json!("hook")]
            }
        );
        assert!(session.take_state_lines().is_empty());
    }
}

#[cfg(debug_assertions)]
#[test]
fn state_second_hook_before_take_is_a_bug() {
    let home = Home::new();
    home.install(
        "goal",
        &(hook(
            "before_tool",
            "state.set('k',1); return {arguments={n=1}}",
            "blocking",
        ) + &hook(
            "before_message",
            "state.set('k',2); return {context='context'}",
            "blocking",
        )),
    );
    let session = home.load(&[]);
    let caught = bounded(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch(&session, "before_tool");
            dispatch(&session, "before_message");
        }))
    });
    let payload = caught.expect_err("second hook must reject an untaken slot");
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    assert_eq!(
        message,
        Some("change lines slot not taken before the next hook call")
    );
}

#[test]
fn state_take_between_hook_calls_passes() {
    let home = Home::new();
    home.install(
        "goal",
        &(hook(
            "before_tool",
            "state.set('k',1); return {arguments={n=1}}",
            "blocking",
        ) + &hook(
            "before_message",
            "state.set('k',2); return {context='context'}",
            "blocking",
        )),
    );
    let session = home.load(&[]);
    for point in ["before_tool", "before_message"] {
        run(&session, point);
        assert_eq!(session.take_change_state().len(), 1);
        assert!(session.take_change_state().is_empty());
    }
}
