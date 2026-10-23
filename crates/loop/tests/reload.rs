//! Reload through the loop's public command and preparation interfaces.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod support;

use contract::ErrorCode;
use contract::events::{CacheLifetime, LoadedExtension, Notice};
use contract::inbox::{Ack, Delivery, Rejection};
use fakes::{Deadline, Scripted};
use r#loop::{PrepareReload, PromptPatch, ReloadPrepared, ReloadSettings};
use std::sync::{Arc, Mutex, mpsc};
use support::{
    DEADLINE, ENDED, REPLY, STEP, Session, assert_kinds, deferred, deferring, delivery, of_kind,
};

use contract::hook::*;

/// A reload setting `{x: false}` rebuilds `x` in full; one removing it
/// rebuilds `x` deferred. A server in `full_servers` rebuilds its tools
/// in full the same way.
#[test]
fn reload_replaces_the_deferrals_whole() {
    let x = Arc::new(support::TestTool::reads("x", "x output"));
    let factory: r#loop::ToolSearchFactory = Arc::new(|_, _| {
        Arc::new(support::TestTool::reads("tool_search", "found")) as Arc<dyn contract::tool::Tool>
    });
    let mut session = Session::wrapped(
        vec![
            Scripted::text("one"),
            Scripted::text("two"),
            Scripted::text("three"),
        ],
        CacheLifetime::FiveMinutes,
        |scripted, _| deferring(scripted),
    );
    session.looped = Some(session.looped.take().unwrap().tool_search(factory));
    let step = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool = Arc::clone(&x);
    attach(
        &mut session,
        Arc::new(move |_| {
            let at = step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut next = prepared();
            next.tools.push(("fx".into(), deferred(Arc::clone(&tool))));
            // Reload 1 turns `x` off by key, reload 2 by server, and
            // reload 3 removes both, so `x` defers again.
            next.prompt.deferrals = match at {
                0 => r#loop::Deferrals {
                    tools: [("x".to_owned(), false)].into_iter().collect(),
                    full_servers: std::collections::BTreeSet::new(),
                },
                1 => r#loop::Deferrals {
                    tools: std::collections::BTreeMap::new(),
                    full_servers: ["fx".to_owned()].into_iter().collect(),
                },
                _ => r#loop::Deferrals::default(),
            };
            Ok(next)
        }),
    );
    let deferred_of = |lines: &[contract::Envelope], name: &str| {
        of_kind(lines, "preamble_built")[0].payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["deferred"]
            .as_bool()
            .unwrap()
    };
    let declared = |lines: &[contract::Envelope]| {
        of_kind(lines, "preamble_built")[0].payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert!(!deferred_of(&lines, "x"));
    assert!(!declared(&lines).contains(&"tool_search".to_owned()));
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert!(!deferred_of(&lines, "x"));
    assert!(!declared(&lines).contains(&"tool_search".to_owned()));
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("three")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert!(deferred_of(&lines, "x"));
    assert!(declared(&lines).contains(&"tool_search".to_owned()));
}

struct StateHooks {
    identity: &'static str,
    installed: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
    pass: Arc<support::CountingHooks>,
    outbox: Mutex<Vec<contract::events::Event>>,
    carried: Mutex<Vec<StateLine>>,
}
impl StateHooks {
    fn record(&self, method: &str) {
        if self.identity == "old" {
            assert!(
                !self.installed.load(std::sync::atomic::Ordering::SeqCst),
                "retired hooks called after installation"
            );
        }
        self.calls
            .lock()
            .unwrap()
            .push(format!("{} {method}", self.identity));
    }
}
impl Hooks for StateHooks {
    fn take_state_lines(&self) -> Vec<contract::events::Event> {
        self.record("take");
        std::mem::take(&mut *self.outbox.lock().unwrap())
    }
    fn admit_state_lines(&self, lines: Vec<StateLine>) -> Vec<contract::events::Event> {
        self.record("admit");
        lines.into_iter().map(StateLine::into_event).collect()
    }
    fn take_change_state(&self) -> Vec<StateLine> {
        self.record("carried");
        std::mem::take(&mut *self.carried.lock().unwrap())
    }
    fn after_tool(&self, call: &AfterToolCall<'_>) -> AfterToolAnswer {
        self.pass.after_tool(call)
    }
    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.pass.before_tool(call)
    }
    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer {
        self.pass.check_tool(call)
    }
    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.record("before_message");
        self.pass.before_message(message)
    }
    fn session_start(&self, call: &SessionStart<'_>) -> ContextAnswer {
        self.pass.session_start(call)
    }
    fn turn_start(&self, call: &TurnStart<'_>) -> ContextAnswer {
        self.pass.turn_start(call)
    }
    fn before_model_call(&self, call: &BeforeModelCall<'_>) -> CheckAnswer {
        self.pass.before_model_call(call)
    }
    fn turn_end(&self, call: &TurnEnd<'_>) -> ContextAnswer {
        self.pass.turn_end(call)
    }
    fn before_handoff(&self, call: &BeforeHandoff<'_>) -> BeforeHandoffAnswer {
        self.pass.before_handoff(call)
    }
    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        self.pass.deliver_to(inbox);
    }
}

#[test]
fn a_shared_prompt_reload_drain_admits_carried_state_only_through_new_hooks() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let carried = StateLine {
        extension: "state".into(),
        key: "carried".into(),
        value: None,
        landing: 1,
    };
    let old = Arc::new(StateHooks {
        identity: "old",
        installed: installed.clone(),
        calls: calls.clone(),
        pass: support::CountingHooks::new(),
        outbox: Mutex::new(Vec::new()),
        carried: Mutex::new(vec![carried]),
    });
    let new = Arc::new(StateHooks {
        identity: "new",
        installed: installed.clone(),
        calls: calls.clone(),
        pass: support::CountingHooks::new(),
        outbox: Mutex::new(vec![contract::events::Event::ExtensionStateUnset(
            contract::events::ExtensionStateUnset {
                extension: "state".into(),
                key: "outbox".into(),
            },
        )]),
        carried: Mutex::new(Vec::new()),
    });
    session.looped = Some(session.looped.take().unwrap().hooks(old));
    attach(
        &mut session,
        Arc::new(move |_| {
            let mut next = prepared();
            next.extensions.clear();
            next.hooks = Some(new.clone());
            let installed = installed.clone();
            next.installed =
                Box::new(move || installed.store(true, std::sync::atomic::Ordering::SeqCst));
            Ok(next)
        }),
    );
    session.inbox.send(delivery("hello")).unwrap();
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "reloaded",
                "extensions_loaded",
                "extension_state_unset",
                "preamble_built",
                "opening_message",
                "extension_state_unset",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let state = of_kind(&lines, "extension_state_unset");
    assert_eq!(state[0].payload["key"], "outbox");
    assert_eq!(state[1].payload["key"], "carried");
    let calls = calls.lock().unwrap();
    assert!(calls.iter().any(|call| call == "old before_message"));
    assert!(calls.iter().any(|call| call == "new take"));
    assert!(calls.iter().any(|call| call == "new admit"));
}

#[test]
fn reload_to_an_empty_extension_set_keeps_replacement_hooks_attached() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let installed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hooks = |identity| {
        Arc::new(StateHooks {
            identity,
            installed: installed.clone(),
            calls: calls.clone(),
            pass: support::CountingHooks::new(),
            outbox: Mutex::new(Vec::new()),
            carried: Mutex::new(Vec::new()),
        })
    };
    session.looped = Some(session.looped.take().unwrap().hooks(hooks("old")));
    let new = hooks("new");
    attach(
        &mut session,
        Arc::new(move |_| {
            let mut next = prepared();
            next.extensions.clear();
            next.hooks = Some(new.clone());
            Ok(next)
        }),
    );
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("hello")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(
        of_kind(&lines, "extensions_loaded")[0].payload["extensions"],
        serde_json::json!([])
    );
    assert!(calls.lock().unwrap().iter().any(|call| call == "new take"));
}

fn prepared() -> ReloadPrepared {
    ReloadPrepared {
        prompt: PromptPatch {
            system: Some("Reloaded system bytes.".into()),
            append: None,
            extensions: Vec::new(),
            extension_dirs: Vec::new(),
            extension_sections: Vec::new(),
            skills_disabled: Vec::new(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            deferrals: Default::default(),
        },
        tools: Vec::new(),
        extensions: vec![LoadedExtension {
            name: "fresh".into(),
            version: "2".into(),
            replaces: Vec::new(),
        }],
        notices: Vec::new(),
        server_sync: r#loop::ServerSync {
            servers: contract::events::ReloadedServers {
                kept: vec!["kept".into()],
                restarted: Vec::new(),
                started: Vec::new(),
                stopped: Vec::new(),
            },
            server_records: Vec::new(),
            server_failures: Vec::new(),
            new_prompts: None,
        },
        hooks: None,
        reviewer: Err(contract::shapes::Failure {
            code: ErrorCode::NoModel,
            message: "no reviewer".into(),
            retry_after_ms: None,
            provider: None,
        }),
        reviewer_observe: None,
        credential_files: Vec::new(),
        settings: ReloadSettings {
            reviewer_notes: "new notes".into(),
            limits: Default::default(),
            retry: Default::default(),
            handoff: Default::default(),
            budget: None,
            idle_exit: None,
            warm_cap: None,
        },
        installed: Box::new(|| {}),
    }
}

fn attach(session: &mut Session, prepare: PrepareReload) {
    session.looped = Some(session.looped.take().unwrap().reloader(prepare));
}

#[test]
fn reload_before_first_turn_builds_once_without_opening_or_skill_changes() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let log = session.log.clone();
    let installed = Arc::new(Mutex::new(Vec::new()));
    let seen = installed.clone();
    attach(
        &mut session,
        Arc::new(move |route| {
            assert_eq!(route.model, support::MODEL);
            assert_eq!(route.credential.as_deref(), Some("work"));
            let mut next = prepared();
            let log = log.clone();
            let seen = seen.clone();
            next.installed = Box::new(move || seen.lock().unwrap().push(log.count()));
            Ok(next)
        }),
    );
    let (tx, rx) = mpsc::channel();
    let log = session.log.clone();
    session
        .inbox
        .send(Delivery::Reload(Ack(Box::new(move |answer| {
            assert_eq!(
                log::read(log.dir()).unwrap().last().unwrap().kind,
                "reloaded"
            );
            tx.send(answer).unwrap();
        }))))
        .unwrap();
    session.inbox.send(delivery("hello")).unwrap();
    session.turn();
    assert!(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(*installed.lock().unwrap(), [1]);
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
            ],
            &["turn_started"],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let reload = of_kind(&lines, "reloaded");
    assert_eq!(
        reload[0].payload["servers"],
        serde_json::json!({"kept":["kept"],"restarted":[],"started":[],"stopped":[]})
    );
    assert_eq!(
        reload[0].payload["extensions"],
        serde_json::json!(["fresh"])
    );
    assert!(!reload[0].payload.contains_key("failed"));
    let built = of_kind(&lines, "preamble_built");
    assert_eq!(built[0].payload["reason"], "reload");
    assert_eq!(
        session.provider.requests()[0].system_prompt,
        "Reloaded system bytes.\n\n# Session\n\nYou are running as fake/model-1."
    );
}

#[test]
fn reload_after_a_turn_rebuilds_and_writes_tolerated_notices_in_order() {
    let mut session = Session::new(vec![Scripted::text("one"), Scripted::text("two")], None);
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    session.lines();
    attach(
        &mut session,
        Arc::new(|_| {
            let mut next = prepared();
            next.notices.push(Notice {
                code: ErrorCode::ExtensionFailed,
                message: "optional extension failed".into(),
                extension: None,
            });
            Ok(next)
        }),
    );
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &["reloaded", "extensions_loaded", "notice", "preamble_built"],
            &["turn_started"],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(
        session.provider.requests()[1].system_prompt,
        "Reloaded system bytes.\n\n# Session\n\nYou are running as fake/model-1."
    );
    assert_eq!(
        of_kind(&lines, "notice")[0].payload["message"],
        "optional extension failed"
    );
}

#[test]
fn removing_a_replacing_extension_restores_the_builtin_and_its_definition() {
    let mut session = Session::new(
        vec![
            support::tool_call_reply("one", &["read"]),
            Scripted::text("one done"),
            support::tool_call_reply("two", &["read"]),
            Scripted::text("two done"),
        ],
        None,
    );
    let builtin = Arc::new(support::TestTool::reads("read", "builtin bytes"));
    let extension = Arc::new(support::TestTool::reads("read", "extension bytes"));
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let b = builtin.clone();
    let e = extension.clone();
    attach(
        &mut session,
        Arc::new(move |_| {
            let mut next = prepared();
            next.tools.push(("builtin".into(), b.clone()));
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                next.tools.push(("extension".into(), e.clone()));
            }
            Ok(next)
        }),
    );
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        of_kind(&lines, "preamble_built")[0].payload["replaced"],
        serde_json::json!([{"name":"read","from":"builtin","to":"extension"}])
    );
    let call = &[
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "text_completed",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "tool_call_completed",
    ];
    assert_kinds(
        &lines,
        &[
            &[
                "session_started",
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
                "turn_started",
            ],
            call,
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(extension.ran().len(), 1);
    assert!(builtin.ran().is_empty());
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "turn_started",
            ],
            call,
            STEP,
            REPLY,
            ENDED,
        ],
    );
    assert_eq!(builtin.ran().len(), 1);
    assert_eq!(extension.ran().len(), 1);
    let built = of_kind(&lines, "preamble_built");
    assert!(built[0].payload.get("replaced").is_none());
    assert_eq!(session.provider.requests()[2].tools[0].name, "read");
}

#[test]
fn a_changed_skill_description_is_sent_once_after_an_intervening_turn_check() {
    let mut session = Session::new(
        vec![
            Scripted::text("one"),
            Scripted::text("two"),
            Scripted::text("three"),
            Scripted::text("four"),
        ],
        None,
    );
    let skill = session.workspace.join(".agents/skills/test");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: test\ndescription: old description\n---\nbody\n",
    )
    .unwrap();
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    session.lines();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: test\ndescription: new description\n---\nbody\n",
    )
    .unwrap();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    assert_kinds(&session.lines(), &[&["turn_started"], STEP, REPLY, ENDED]);
    attach(&mut session, Arc::new(|_| Ok(prepared())));
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("three")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            &[
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "skills_changed",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
    let changed = of_kind(&lines, "skills_changed");
    assert_eq!(
        changed[0].payload["added"][0]["description"],
        "new description"
    );
    session
        .inbox
        .send(Delivery::Reload(support::ignore()))
        .unwrap();
    session.inbox.send(delivery("four")).unwrap();
    session.turn();
    assert_kinds(
        &session.lines(),
        &[
            &[
                "reloaded",
                "extensions_loaded",
                "preamble_built",
                "turn_started",
            ],
            STEP,
            REPLY,
            ENDED,
        ],
    );
}

#[test]
fn post_offer_load_rebuilds_only_when_it_registers_a_tool() {
    for adds_tool in [false, true] {
        let mut session = Session::new(vec![Scripted::text("done")], None);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = calls.clone();
        session.looped = Some(
            session
                .looped
                .take()
                .unwrap()
                .approved_code(Arc::new(move |_| {
                    called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(r#loop::Approved {
                        tools: if adds_tool {
                            vec![(
                                "repository".into(),
                                Arc::new(support::TestTool::reads("late", "late"))
                                    as Arc<dyn contract::tool::Tool>,
                            )]
                        } else {
                            Vec::new()
                        },
                        events: Vec::new(),
                    })
                })),
        );
        attach(&mut session, Arc::new(|_| Ok(prepared())));
        session
            .inbox
            .send(Delivery::Reload(support::ignore()))
            .unwrap();
        session.inbox.send(delivery("hello")).unwrap();
        session.turn();
        let lines = session.lines();
        let mut prefix = vec![
            "session_started",
            "reloaded",
            "extensions_loaded",
            "preamble_built",
        ];
        if adds_tool {
            prefix.push("preamble_built");
        }
        prefix.extend(["opening_message", "turn_started"]);
        assert_kinds(&lines, &[&prefix, STEP, REPLY, ENDED]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let request = &session.provider.requests()[0];
        assert_eq!(
            request.tools.iter().any(|tool| tool.name == "late"),
            adds_tool
        );
        for line in of_kind(&lines, "preamble_built") {
            assert_eq!(line.payload["reason"], "reload");
        }
    }
}

#[test]
fn rejected_preparation_keeps_the_preamble_and_allows_a_later_reload() {
    for code in [ErrorCode::ConfigInvalid, ErrorCode::IoFailed] {
        let mut session = Session::new(vec![Scripted::text("one"), Scripted::text("two")], None);
        let calls = Arc::new(Mutex::new(Vec::new()));
        session.looped = Some(session.looped.take().unwrap().hooks(Arc::new(StateHooks {
            identity: "old",
            installed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            calls: calls.clone(),
            pass: support::CountingHooks::new(),
            outbox: Mutex::new(Vec::new()),
            carried: Mutex::new(Vec::new()),
        })));
        session.inbox.send(delivery("one")).unwrap();
        session.turn();
        session.lines();
        let old = session.provider.requests()[0].system_prompt.clone();
        calls.lock().unwrap().clear();
        let rejected = code.clone();
        attach(
            &mut session,
            Arc::new(move |_| {
                Err(Rejection {
                    code: rejected.clone(),
                    message: "bad configuration file".into(),
                })
            }),
        );
        let (tx, rx) = mpsc::channel();
        session
            .inbox
            .send(Delivery::Reload(Ack(Box::new(move |answer| {
                tx.send(answer).unwrap()
            }))))
            .unwrap();
        session.inbox.send(delivery("two")).unwrap();
        session.turn();
        assert_eq!(
            Deadline::after(DEADLINE)
                .recv(&rx)
                .unwrap()
                .unwrap_err()
                .code,
            code
        );
        assert_kinds(&session.lines(), &[&["turn_started"], STEP, REPLY, ENDED]);
        assert_eq!(session.provider.requests()[1].system_prompt, old);
        assert!(calls.lock().unwrap().iter().any(|call| call == "old take"));
    }
}
