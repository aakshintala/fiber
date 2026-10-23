//! Admission, atomic preparation and fatal writes at the reload boundary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use super::*;
use contract::events::PreambleReason;
use contract::inbox::Answer;
use contract::rules::{Rules, RulesError, StandingRules};
use contract::{ErrorCode, SessionId, TurnId};
use fakes::clock::FakeClock;
use fakes::{Deadline, Scripted, ScriptedProvider};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};

thread_local! { static FAIL_RELOAD_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(super) fn fail_reload_write() -> bool {
    FAIL_RELOAD_WRITE.with(|fail| fail.replace(false))
}

#[test]
fn failed_reload_append_drops_the_ack_without_accepting_it() {
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| Ok(prepared())));
    FAIL_RELOAD_WRITE.with(|fail| fail.set(true));
    let (ack, rx) = ack();
    assert!(w.looped.take_reload(ack).is_err());
    assert!(matches!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&rx),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

#[test]
fn a_deferred_reload_runs_once_before_the_next_prompt() {
    let mut w = world();
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    w.looped = w.looped.reloader(Arc::new(move |_| {
        called.fetch_add(1, Ordering::SeqCst);
        Ok(prepared())
    }));
    let (ack, rx) = ack();
    let offer = contract::events::RepositoryCodeOffered {
        request_id: contract::RequestId("r_reload".into()),
        items: Vec::new(),
    };
    let mut folded = crate::offer::Folded::default();
    folded.fold(&contract::events::Event::RepositoryCodeOffered(
        offer.clone(),
    ));
    w.looped.repository = crate::offer::State::resumed(folded);
    w.looped
        .log
        .append(
            &contract::events::Event::Clients(contract::events::Clients { count: 1 }),
            None,
            None,
        )
        .unwrap();
    w.looped.answerable = true;
    w.looped = w.looped.repository_code(Arc::new(EmptyRepository));
    w.inbox
        .send(contract::inbox::Delivery::Reload(ack))
        .unwrap();
    w.inbox
        .send(contract::inbox::Delivery::Reply(
            contract::commands::Reply {
                request_id: offer.request_id,
                answer: contract::commands::ReplyAnswer::Decisions {
                    decisions: Vec::new(),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
    assert!(w.looped.reraise_offer().unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        w.looped.deferred.front(),
        Some(contract::inbox::Delivery::Reload(_))
    ));
    w.inbox
        .send(contract::inbox::Delivery::Close(Ack(Box::new(|_| {}))))
        .unwrap();
    assert!(w.looped.turn().unwrap().is_none());
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // `docs/events.md` promises `session_started` first, an offer before its
    // `repository_code_resolved`, and `extensions_loaded` and `preamble_built`
    // after `reloaded` ("Repository code", "`reloaded`", "`extensions_loaded`");
    // it leaves the rest open, so the rest is a set.
    let seen = kinds(&w.looped);
    let at = |name: &str| seen.iter().position(|kind| kind == name).unwrap();
    assert_eq!(seen[0], "session_started");
    assert!(at("repository_code_offered") < at("repository_code_resolved"));
    assert!(at("reloaded") < at("extensions_loaded"));
    assert!(at("reloaded") < at("preamble_built"));
    let mut set = seen.clone();
    set.sort();
    assert_eq!(
        set,
        [
            "extensions_loaded",
            "preamble_built",
            "reloaded",
            "repository_code_offered",
            "repository_code_resolved",
            "session_started"
        ]
    );
}
struct EmptyRepository;
impl contract::repository::RepositoryCode for EmptyRepository {
    fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
        Err(contract::shapes::Failure {
            code: contract::ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }
    fn unapproved(&self) -> Result<Vec<contract::repository::Unapproved>, Failure> {
        Ok(Vec::new())
    }
    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<contract::repository::Decided, Failure> {
        unreachable!("the pending offer has no items")
    }
}
struct EmptyRules;
impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }
    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

struct World {
    looped: Loop,
    inbox: mpsc::Sender<contract::inbox::Delivery>,
    _home: fakes::TempDir,
}
fn world() -> World {
    let home = fakes::TempDir::new("fiber-reload");
    let clock = FakeClock::new();
    let log = Arc::new(
        log::Log::create(home.path(), SessionId("s_reload".into()), clock.clone()).unwrap(),
    );
    let (sender, inbox) = mpsc::channel();
    let looped = Loop::start(
        crate::Session {
            log,
            inbox,
            provider: Arc::new(ScriptedProvider::new([Scripted::text("done")])),
            model: crate::Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::PromptInputs::new(
                home.path().into(),
                "/bin/sh".into(),
                "events.jsonl".into(),
                clock,
                fakes::CONTEXT_WINDOW,
            ),
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: home.path().display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        None,
    )
    .unwrap();
    World {
        looped,
        inbox: sender,
        _home: home,
    }
}
fn prepared() -> ReloadPrepared {
    ReloadPrepared {
        prompt: PromptPatch {
            system: Some("new system".into()),
            append: None,
            extensions: Vec::new(),
            extension_dirs: Vec::new(),
            extension_sections: Vec::new(),
            skills_disabled: Vec::new(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            deferrals: Default::default(),
        },
        tools: Vec::new(),
        extensions: Vec::new(),
        notices: Vec::new(),
        server_sync: ServerSync {
            servers: contract::events::ReloadedServers {
                kept: Vec::new(),
                restarted: Vec::new(),
                started: Vec::new(),
                stopped: Vec::new(),
            },
            server_records: Vec::new(),
            server_failures: Vec::new(),
            new_prompts: None,
        },
        hooks: None,
        reviewer: Err(Failure {
            code: ErrorCode::NoModel,
            message: "new reviewer failure".into(),
            retry_after_ms: None,
            provider: None,
        }),
        reviewer_observe: None,
        credential_files: vec![PathBuf::from("/read/credential")],
        settings: ReloadSettings {
            reviewer_notes: "new notes".into(),
            limits: BlockLimits::default(),
            retry: Retry::default(),
            handoff: HandoffSettings::default(),
            budget: Some(10.0),
            idle_exit: Some(Duration::from_secs(100)),
            warm_cap: Some(2),
        },
        installed: Box::new(|| {}),
    }
}
fn ack() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}
fn kinds(looped: &Loop) -> Vec<String> {
    log::read(looped.log.dir())
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

/// Hooks holding one recorded state line: `take_state_lines` hands it
/// out once, as the late preparation's fresh hooks would a turn that ran
/// while the await was queued.
struct StatefulHooks {
    state: Mutex<Vec<contract::events::Event>>,
}

impl contract::hook::Hooks for StatefulHooks {
    fn take_state_lines(&self) -> Vec<contract::events::Event> {
        std::mem::take(&mut *self.state.lock().unwrap())
    }
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }
    fn before_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }
    fn check_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }
    fn before_message(
        &self,
        _message: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }
    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }
    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }
    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<contract::inbox::Delivery>) {}
}

#[test]
fn a_late_apply_drains_the_prepared_hooks_state() {
    let mut w = world();
    let hooks = Arc::new(StatefulHooks {
        state: Mutex::new(vec![contract::events::Event::Notice(
            contract::events::Notice {
                code: ErrorCode::IoFailed,
                message: "a turn that ran while the await was queued".into(),
                extension: None,
            },
        )]),
    });
    let mut prep = prepared();
    prep.hooks = Some(hooks.clone());
    w.looped.apply_late(prep).unwrap();
    assert!(
        hooks.state.lock().unwrap().is_empty(),
        "the drain takes the recorded state, as `apply_reload` does"
    );
    assert_eq!(kinds(&w.looped), ["session_started", "extensions_loaded"]);
}

#[test]
fn closing_busy_and_rewound_refuse_without_preparing_or_writing() {
    for state in ["closing", "busy", "rewound"] {
        let mut w = world();
        let called = Arc::new(AtomicUsize::new(0));
        let calls = called.clone();
        w.looped = w.looped.reloader(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(prepared())
        }));
        let (ack, rx) = ack();
        match state {
            "closing" => {
                w.looped.closing = true;
                w.looped.take_reload(ack).unwrap();
            }
            "busy" => w
                .looped
                .admit_running(
                    contract::inbox::Delivery::Reload(ack),
                    &TurnId("t_busy".into()),
                )
                .unwrap(),
            _ => {
                w.looped.rewound = true;
                w.looped
                    .admit_idle(
                        contract::inbox::Delivery::Reload(ack),
                        &mut crate::inbox::TurnInput::of(Vec::new()),
                    )
                    .unwrap();
            }
        }
        let rejection = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap_err();
        if state == "busy" {
            assert_eq!(rejection.message, "A turn is running; reload once it ends.");
        }
        assert_eq!(
            rejection.code,
            if state == "busy" {
                ErrorCode::Busy
            } else {
                ErrorCode::Closing
            }
        );
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert_eq!(kinds(&w.looped), ["session_started"]);
    }
}

#[test]
fn no_reloader_is_invalid_arguments() {
    let mut w = world();
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

#[test]
fn applied_reload_swaps_settings_before_installation_and_accepts_after_the_event() {
    let mut w = world();
    let log = w.looped.log.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    w.looped = w.looped.reloader(Arc::new(move |_| {
        let mut next = prepared();
        let seen = recorded.clone();
        let log = log.clone();
        next.installed = Box::new(move || seen.lock().unwrap().push(log.count()));
        Ok(next)
    }));
    let (tx, rx) = mpsc::channel();
    let log = w.looped.log.clone();
    w.looped
        .take_reload(Ack(Box::new(move |answer| {
            assert_eq!(log.count(), 2);
            tx.send(answer).unwrap();
        })))
        .unwrap();
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(*seen.lock().unwrap(), [1]);
    assert_eq!(
        kinds(&w.looped),
        [
            "session_started",
            "reloaded",
            "extensions_loaded",
            "preamble_built"
        ]
    );
    assert_eq!(w.looped.preamble_reason, PreambleReason::Reload);
    assert_eq!(w.looped.prompt.system.as_deref(), Some("new system"));
    assert_eq!(w.looped.prompt.cache_lifetime, CacheLifetime::FiveMinutes);
    assert_eq!(w.looped.reviewer_notes, "new notes");
    assert_eq!(
        w.looped.reviewer.as_ref().err().unwrap().message,
        "new reviewer failure"
    );
    assert_eq!(w.looped.reviewer_sent, None);
    assert_eq!(
        w.looped.credential_files,
        [PathBuf::from("/read/credential")]
    );
    assert_eq!(w.looped.budget, Some(10.0));
    assert_eq!(w.looped.idle_exit, Some(Duration::from_secs(100)));
    assert!(!w.looped.opened);
}

#[test]
fn failed_preamble_append_ends_after_the_reload_ack_is_accepted() {
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| Ok(prepared())));
    crate::timing_tests::arm_fail_preamble_write();
    let (ack, rx) = ack();
    assert!(w.looped.take_reload(ack).is_err());
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        kinds(&w.looped),
        ["session_started", "reloaded", "extensions_loaded"]
    );
}

#[test]
fn a_preparer_cancel_rejects_without_applying() {
    // debt: Part 1 tests the cancellation rejection only; Part 2's shutdown
    // wiring must prove that cancelling the prepare wait ends the loop.
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| {
        Err(Rejection {
            code: ErrorCode::Closing,
            message: "shutdown".into(),
        })
    }));
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::Closing
    );
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

/// A tool that only names itself: the reconnect merge test registers
/// and removes it without ever calling it.
struct NamedTool(&'static str);

impl contract::tool::Tool for NamedTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.0.into(),
            description: "reconnect test tool".into(),
            input_schema: serde_json::Value::Object(serde_json::Map::new()),
            deferred: false,
            hosted: None,
        }
    }
    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        panic!("never called")
    }
    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("never called")
    }
}

#[test]
fn take_reconnect_without_a_reconnector_rejects_invalid_arguments() {
    let mut w = world();
    let (ack, rx) = ack();
    w.looped.take_reconnect("db".into(), ack).unwrap();
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::InvalidArguments
    );
    assert_eq!(kinds(&w.looped), ["session_started"]);
}

#[test]
fn take_reconnect_merges_the_registry_and_writes_the_records_before_accepting() {
    let mut w = world();
    let stale = Arc::new(NamedTool("db__old"));
    w.looped.tools.insert(
        "db__old".into(),
        (
            "db".into(),
            Arc::clone(&stale) as Arc<dyn contract::tool::Tool>,
            stale.definition(),
        ),
    );
    w.looped = w.looped.reconnector(Arc::new(|server| {
        assert_eq!(server, "db");
        let fresh = Arc::new(NamedTool("db__list"));
        Ok(Reconnected {
            tools: vec![(
                "db".into(),
                Arc::clone(&fresh) as Arc<dyn contract::tool::Tool>,
            )],
            removed: vec!["db__old".into()],
            records: vec![contract::tool::ServerRecord::Ready(
                contract::events::McpServerReady {
                    server: "db".into(),
                },
            )],
        })
    }));
    let (ack, rx) = ack();
    w.looped.take_reconnect("db".into(), ack).unwrap();
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .is_ok()
    );
    assert!(w.looped.tools.contains_key("db__list"));
    assert!(!w.looped.tools.contains_key("db__old"));
    assert!(kinds(&w.looped).contains(&"mcp_server_ready".to_owned()));
}

#[test]
fn applied_reload_writes_server_records_before_reloaded_with_the_grouping() {
    use contract::events::{McpServerFailed, McpServerReady, ReloadFailure, ServerFailure};
    let mut w = world();
    w.looped = w.looped.reloader(Arc::new(|_| {
        let mut next = prepared();
        next.server_sync.servers.kept = vec!["keep".into()];
        next.server_sync.servers.restarted = vec!["changed".into()];
        next.server_sync.servers.started = vec!["new".into()];
        next.server_sync.servers.stopped = vec!["gone".into()];
        next.server_sync.server_records = vec![
            contract::tool::ServerRecord::Ready(McpServerReady {
                server: "changed".into(),
            }),
            contract::tool::ServerRecord::Failed(McpServerFailed {
                server: "flaky".into(),
                reason: ServerFailure::StartFailed,
                will_restart: false,
                error: Failure {
                    code: ErrorCode::McpServerUnavailable,
                    message: "the start failed".into(),
                    retry_after_ms: None,
                    provider: None,
                },
            }),
        ];
        next.server_sync.server_failures = vec![ReloadFailure {
            server: "flaky".into(),
            reason: ServerFailure::StartFailed,
            error: Failure {
                code: ErrorCode::McpServerUnavailable,
                message: "the start failed".into(),
                retry_after_ms: None,
                provider: None,
            },
        }];
        Ok(next)
    }));
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        kinds(&w.looped),
        [
            "session_started",
            "mcp_server_ready",
            "mcp_server_failed",
            "reloaded",
            "extensions_loaded",
            "preamble_built"
        ]
    );
    let lines: Vec<_> = log::read(w.looped.log.dir())
        .unwrap()
        .into_iter()
        .map(|line| {
            contract::events::Event::from_envelope(&line)
                .unwrap()
                .unwrap()
        })
        .collect();
    let mut found = None;
    for event in &lines {
        if let contract::events::Event::Reloaded(reloaded) = event {
            found = Some(reloaded);
            break;
        }
    }
    let reloaded = found.unwrap();
    assert_eq!(
        reloaded.servers,
        contract::events::ReloadedServers {
            kept: vec!["keep".into()],
            restarted: vec!["changed".into()],
            started: vec!["new".into()],
            stopped: vec!["gone".into()],
        }
    );
    let failed = reloaded.failed.as_ref().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].server, "flaky");
    assert_eq!(failed[0].reason, ServerFailure::StartFailed);
}

#[test]
fn an_appended_prompt_row_fetches_through_the_carried_sources() {
    use contract::events::CommandInfo;
    use contract::shapes::ContentPart;
    fn row(tag: &str, name: &str) -> CommandInfo {
        CommandInfo {
            name: name.into(),
            description: "A prompt.".into(),
            argument_hint: None,
            tag: tag.into(),
            extension: false,
        }
    }
    let old: crate::FetchPrompt = Arc::new(|_, _, _, _| contract::tool::Output {
        error: Some(Failure {
            code: ErrorCode::InvalidArguments,
            message: "the startup fetch".into(),
            retry_after_ms: None,
            provider: None,
        }),
        ..contract::tool::Output::default()
    });
    let extra: crate::FetchPrompt = Arc::new(|server, prompt, _, _| {
        assert_eq!(server, "new");
        assert_eq!(prompt, "greet");
        contract::tool::Output {
            content: vec![ContentPart::Text { text: "hi".into() }],
            ..contract::tool::Output::default()
        }
    });
    let mut w = world();
    w.looped = w.looped.server_prompts(crate::ServerPrompts {
        rows: vec![row("old", "before")],
        fetch: old,
    });
    w.looped = w.looped.reloader(Arc::new(move |_| {
        let mut p = prepared();
        // Rows and fetch travel as one value: new rows without their
        // fetch cannot be represented, so appended rows always answer
        // through the carried sources.
        p.server_sync.new_prompts = Some((vec![row("new", "greet")], Arc::clone(&extra)));
        Ok(p)
    }));
    let (ack, rx) = ack();
    w.looped.take_reload(ack).unwrap();
    assert!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .unwrap()
            .unwrap()
            .is_none()
    );
    let prompts = w
        .looped
        .server_prompts
        .as_ref()
        .expect("prompts stay installed");
    assert_eq!(
        prompts.rows.len(),
        2,
        "the appended row joins, every startup row stays"
    );
    let cancel = crate::TurnCancel::default();
    let fetched = (prompts.fetch)("new", "greet", "", &cancel);
    assert!(fetched.error.is_none());
    assert!(matches!(&fetched.content[..], [ContentPart::Text { text }] if text == "hi"));
    let kept = (prompts.fetch)("old", "before", "", &cancel);
    assert_eq!(
        kept.error.map(|error| error.code),
        Some(ErrorCode::InvalidArguments)
    );
}

// Reload, round 23: every comparison of the prompt-row merge is
// independently decisive — the first of each tag-and-name wins, and any
// other combination appends.

#[test]
fn merge_command_rows_keeps_the_first_of_each_tag_and_name() {
    // Bites the three merge-predicate mutants (`tag ==`→`!=`, `&&`→`||`,
    // `name ==`→`!=`): an identical row appends nothing, while a row
    // differing in either comparison appends. Each mutation misfiles at
    // least one row below.
    use contract::events::CommandInfo;
    fn row(tag: &str, name: &str) -> CommandInfo {
        CommandInfo {
            name: name.into(),
            description: "A prompt.".into(),
            argument_hint: None,
            tag: tag.into(),
            extension: false,
        }
    }
    let mut rows = vec![row("old", "before")];
    ServerSync::merge_command_rows(&mut rows, &[row("old", "before")]);
    assert_eq!(rows.len(), 1, "an identical row appends nothing");
    let mut rows = vec![row("old", "before")];
    ServerSync::merge_command_rows(&mut rows, &[row("old", "after")]);
    assert_eq!(rows.len(), 2, "the same tag with a new name appends");
    let mut rows = vec![row("old", "before")];
    ServerSync::merge_command_rows(&mut rows, &[row("new", "before")]);
    assert_eq!(rows.len(), 2, "a new tag with the same name appends");
    let mut rows = vec![row("old", "before")];
    ServerSync::merge_command_rows(&mut rows, &[row("new", "after")]);
    assert_eq!(rows.len(), 2, "a new tag with a new name appends");
}
