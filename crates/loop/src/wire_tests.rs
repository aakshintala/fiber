//! Loop route observation: the session factory's attempt ordinals, the
//! reviewer's own factory, switches applying and clearing factories, and
//! warming through the session route. Capture factories wrap the existing
//! scripted providers, leaving `Provider::call` unchanged; the shared
//! writer and peak helpers are covered in `diag_tests`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::clock::Clock as _;
use contract::diag::Observe;
use contract::emit::Emit;
use contract::events::{CacheLifetime, ToolCallRequested, TurnOutcome};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, ModelCall, ModelRequest, Provider};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, ErrorCode, SessionId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use serde_json::{Map, Value, json};

use crate::{BlockLimits, Loop, Model, Reviewer};

/// How long a test waits for a turn, a handshake or a parked wait.
const WAIT: Duration = Duration::from_secs(10);

/// Standing rules that allow nothing and remember nothing.
#[derive(Default)]
struct EmptyRules;

impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// A tool whose calls execute: the reviewer judges them.
struct ExecTool;

impl Tool for ExecTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "shell".into(),
            description: "Runs.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: true,
                paths: None,
            },
            subject: None,
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "ran it".into(),
            }],
            ..Output::default()
        }
    }
}

fn failed(code: ErrorCode) -> Scripted {
    Scripted::failed(Failure {
        code,
        message: "The call failed.".into(),
        retry_after_ms: None,
        provider: None,
    })
}

fn tool_reply(name: &str, arguments: Value) -> Scripted {
    let mut end = fakes::reply("");
    end.actions.push(contract::provider::ReplyAction::ToolCall(
        ToolCallRequested {
            name: name.into(),
            arguments: arguments.clone(),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
    ));
    Scripted {
        deltas: vec![Delta::ToolCallArguments(
            contract::events::ToolCallArgumentsDelta {
                index: 0,
                name: Some(name.into()),
                text: arguments.to_string(),
            },
        )],
        end: Ok(end),
    }
}

fn no_wait() -> crate::Retry {
    crate::Retry {
        attempts: 3,
        initial: Duration::ZERO,
        max: Duration::ZERO,
    }
}

/// An observed factory over `inner`, recording every attempt it builds
/// and handshaking each entry through `gate` when one is given.
fn gated(
    attempts: &Arc<Mutex<Vec<u32>>>,
    inner: Arc<ScriptedProvider>,
    gate: Option<Gate>,
) -> Observe {
    let attempts = Arc::clone(attempts);
    // Behind a lock: a channel receiver is `Send` but not `Sync`, and the
    // factory is shared.
    let gate = gate.map(Mutex::new);
    Arc::new(move |request: &ModelRequest, attempt: NonZeroU32| {
        attempts.lock().unwrap().push(attempt.get());
        if let Some(gate) = &gate {
            let gate = gate.lock().unwrap();
            gate.entered.send(()).unwrap();
            Deadline::after(WAIT)
                .recv(&gate.release)
                .expect("the test releases the call");
        }
        inner.call(request)
    })
}

struct World {
    home: fakes::TempDir,
    clock: Arc<fakes::clock::FakeClock>,
    provider: Arc<ScriptedProvider>,
    reviewer: Option<Arc<ScriptedProvider>>,
    attempts: Arc<Mutex<Vec<u32>>>,
    reviewer_attempts: Arc<Mutex<Vec<u32>>>,
    inbox_tx: mpsc::Sender<Delivery>,
    cancel: Arc<crate::TurnCancel>,
    looped: Loop,
}

struct Build {
    script: Vec<Scripted>,
    tools: Vec<Arc<dyn Tool>>,
    reviewer_script: Option<Vec<Scripted>>,
    retry: Option<crate::Retry>,
    warm: Option<u32>,
    answerable: bool,
    observe: bool,
    session_gate: Option<Gate>,
    reviewer_gate: Option<Gate>,
    switch: Option<SwitchRig>,
}

/// A bounded handshake around a factory: the factory signals each entry
/// and waits for the test's release, so the test forces the interleaving
/// instead of trusting scheduling.
struct Gate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

fn gate() -> (Gate, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    (
        Gate {
            entered: entered_tx,
            release: release_rx,
        },
        entered_rx,
        release_tx,
    )
}

fn model(reference: &str) -> Model {
    Model {
        reference: reference.into(),
        cost: None,
        subscription: false,
    }
}

fn build(build: Build) -> World {
    let home = fakes::TempDir::new("fiber-wire");
    let workspace = home.path().join("workspace");
    let credentials = home.path().join("credentials");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&credentials).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let dyn_clock = Arc::clone(&clock) as Arc<dyn contract::clock::Clock>;
    let log = Arc::new(
        log::Log::create(
            home.path(),
            SessionId("s_test".into()),
            Arc::clone(&dyn_clock),
        )
        .unwrap(),
    );
    let provider = Arc::new(ScriptedProvider::new(build.script));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let reviewer_attempts = Arc::new(Mutex::new(Vec::new()));
    let (inbox_tx, inbox_rx) = mpsc::channel();
    let cancel = Arc::new(crate::TurnCancel::default());
    let rules: Arc<dyn Rules> = Arc::new(EmptyRules);
    let mut looped = Loop::start(
        crate::Session {
            log,
            provider: Arc::clone(&provider) as Arc<dyn Provider>,
            model: model("fake/model"),
            prompt: crate::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&dyn_clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: inbox_rx,
            tools: build
                .tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules,
            },
        },
        None,
    )
    .unwrap()
    .cancelled_by(Arc::clone(&cancel))
    .answerable(build.answerable);
    if build.observe {
        looped = looped.observed(Some(gated(
            &attempts,
            Arc::clone(&provider),
            build.session_gate,
        )));
    }
    if let Some(retry) = build.retry {
        looped = looped.retry(retry);
    }
    if let Some(cap) = build.warm {
        looped = looped.warm(Some(cap));
    }
    if let Some(rig) = &build.switch {
        looped = looped.switcher(switcher(rig), crate::Switchable { chosen: None });
    }
    let mut reviewer_provider = None;
    if let Some(script) = build.reviewer_script {
        let reviewer = Arc::new(ScriptedProvider::new(script));
        let observe = gated(
            &reviewer_attempts,
            Arc::clone(&reviewer),
            build.reviewer_gate,
        );
        looped = looped
            .reviewer(
                Ok(Reviewer {
                    provider: Arc::clone(&reviewer) as Arc<dyn Provider>,
                    model: model("fake/reviewer"),
                    cache_lifetime: CacheLifetime::OneHour,
                    context_window: fakes::CONTEXT_WINDOW,
                    thinking_levels: Vec::new(),
                }),
                BlockLimits::default(),
            )
            .reviewer_observed(Some(observe));
        reviewer_provider = Some(reviewer);
    }
    World {
        home,
        clock,
        provider,
        reviewer: reviewer_provider,
        attempts,
        reviewer_attempts,
        inbox_tx,
        cancel,
        looped,
    }
}

fn plain_build() -> Build {
    Build {
        script: Vec::new(),
        tools: Vec::new(),
        reviewer_script: None,
        retry: None,
        warm: None,
        answerable: true,
        observe: true,
        session_gate: None,
        reviewer_gate: None,
        switch: None,
    }
}

fn prompt(world: &World, text: &str) {
    world
        .inbox_tx
        .send(Delivery::Prompt(
            Message {
                content: vec![ContentPart::Text { text: text.into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_go".into())),
                },
            },
            Ack(Box::new(|_| {})),
        ))
        .unwrap();
}

fn turn(world: &mut World) -> Option<TurnOutcome> {
    world.looped.turn().unwrap()
}

fn kinds(world: &World) -> Vec<String> {
    log::read(&world.home.path().join("s_test"))
        .unwrap()
        .iter()
        .map(|line| line.kind.clone())
        .collect()
}

#[test]
fn a_retry_then_a_later_call_records_one_two_then_one() {
    let script = || {
        vec![
            failed(ErrorCode::RateLimited),
            Scripted::text("Recovered."),
            Scripted::text("Second."),
        ]
    };
    let mut observed = build(Build {
        script: script(),
        retry: Some(no_wait()),
        ..plain_build()
    });
    prompt(&observed, "hi");
    assert_eq!(turn(&mut observed), Some(TurnOutcome::Completed));
    assert_eq!(*observed.attempts.lock().unwrap(), vec![1, 2]);
    prompt(&observed, "again");
    assert_eq!(turn(&mut observed), Some(TurnOutcome::Completed));
    assert_eq!(*observed.attempts.lock().unwrap(), vec![1, 2, 1]);
    // The same script unobserved writes the same durable lines.
    let mut plain = build(Build {
        script: script(),
        retry: Some(no_wait()),
        observe: false,
        ..plain_build()
    });
    prompt(&plain, "hi");
    assert_eq!(turn(&mut plain), Some(TurnOutcome::Completed));
    prompt(&plain, "again");
    assert_eq!(turn(&mut plain), Some(TurnOutcome::Completed));
    // Session ids are one world apart, so compare kinds only.
    let mut observed_kinds = kinds(&observed);
    let mut plain_kinds = kinds(&plain);
    observed_kinds.retain(|kind| kind != "session_started");
    plain_kinds.retain(|kind| kind != "session_started");
    assert_eq!(observed_kinds, plain_kinds);
}

#[test]
fn an_exhausted_retry_invents_no_further_construction() {
    let mut world = build(Build {
        script: vec![
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
            failed(ErrorCode::RateLimited),
        ],
        retry: Some(crate::Retry {
            attempts: 2,
            initial: Duration::ZERO,
            max: Duration::ZERO,
        }),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Failed));
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 2, 3]);
}

#[test]
fn a_cancel_during_the_backoff_invents_no_further_construction() {
    let mut world = build(Build {
        script: vec![failed(ErrorCode::RateLimited), Scripted::text("Unused.")],
        retry: Some(crate::Retry {
            attempts: 3,
            initial: Duration::from_secs(2),
            max: Duration::from_secs(60),
        }),
        ..plain_build()
    });
    prompt(&world, "hi");
    let clock = Arc::clone(&world.clock);
    let cancel = Arc::clone(&world.cancel);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| world.looped.turn());
        // The retry waits two seconds out: cancel only once the wait has
        // started, so the test never races the failing call.
        let until = clock.origin() + Duration::from_secs(2);
        assert!(
            clock.await_parked(until, fakes::Deadline::after(WAIT)),
            "the retry parks its backoff before the cancel lands"
        );
        cancel.cancel();
        let outcome = turn.join().unwrap().unwrap();
        assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    });
    assert_eq!(*world.attempts.lock().unwrap(), vec![1]);
}

#[test]
fn review_calls_use_the_reviewer_factory_at_the_first_attempt() {
    let (session_gate, session_entered, session_release) = gate();
    let (reviewer_gate, reviewer_entered, reviewer_release) = gate();
    let mut world = build(Build {
        script: vec![tool_reply("shell", json!({})), Scripted::text("Done.")],
        tools: vec![Arc::new(ExecTool)],
        reviewer_script: Some(vec![Scripted::text("allow")]),
        session_gate: Some(session_gate),
        reviewer_gate: Some(reviewer_gate),
        ..plain_build()
    });
    prompt(&world, "go");
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| world.looped.turn().unwrap());
        // The turn's call enters the session factory first, and the review
        // waits for its release: nothing jumps ahead of the session route.
        Deadline::after(WAIT)
            .recv(&session_entered)
            .expect("the turn's call entered");
        assert!(
            reviewer_entered.try_recv().is_err(),
            "no review runs before the turn's call is released"
        );
        session_release.send(()).unwrap();
        // The review enters the reviewer's factory, never the session's.
        Deadline::after(WAIT)
            .recv(&reviewer_entered)
            .expect("the review entered");
        assert!(
            session_entered.try_recv().is_err(),
            "the review builds through its own factory"
        );
        reviewer_release.send(()).unwrap();
        // The tool ran; the next step's call returns to the session route.
        Deadline::after(WAIT)
            .recv(&session_entered)
            .expect("the next step entered");
        session_release.send(()).unwrap();
        assert_eq!(turn.join().unwrap(), Some(TurnOutcome::Completed));
    });
    assert_eq!(
        *world.attempts.lock().unwrap(),
        vec![1, 1],
        "each step's first call builds at 1"
    );
    assert_eq!(
        *world.reviewer_attempts.lock().unwrap(),
        vec![1],
        "the first-stage review builds at 1"
    );
    // Contexts do not cross: the session's requests carry the session key,
    // the reviewer's the reviewer key.
    let session_keys: Vec<String> = world
        .provider
        .requests()
        .iter()
        .map(|request| request.cache_key.clone())
        .collect();
    assert_eq!(session_keys, vec!["s_test".to_owned(), "s_test".to_owned()]);
    let reviewer = world.reviewer.expect("a reviewer provider");
    let reviewer_keys: Vec<String> = reviewer
        .requests()
        .iter()
        .map(|request| request.cache_key.clone())
        .collect();
    assert_eq!(reviewer_keys, vec!["s_test:reviewer".to_owned()]);
}

#[test]
fn a_warming_refresh_uses_the_session_factory_at_the_first_attempt() {
    let mut world = build(Build {
        // One reply for the turn, one for the refresh.
        script: vec![Scripted::text("Warm."), Scripted::text("Warmer.")],
        warm: Some(1),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(*world.attempts.lock().unwrap(), vec![1]);
    // Past the refresh's due point but before the cache ends: the held
    // request is the turn's own, recorded on its signal.
    let start = world.clock.now();
    let stop = world.looped.warm_stop(start).expect("the session warms");
    // No code under test waits on the clock here; the refresh compares its due point only (ruling 6179406086).
    world.clock.advance(Duration::from_secs(3575));
    let refreshed = world.looped.refresh(stop).unwrap();
    assert!(
        matches!(refreshed, crate::warm::Refreshed::Sent),
        "the due refresh sent"
    );
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 1]);
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].max_output_tokens,
        Some(1),
        "the refresh caps its output at one token"
    );
}

#[test]
fn a_step_hands_the_provider_and_the_warm_hold_one_conversation() {
    let mut world = build(Build {
        warm: Some(1),
        script: vec![Scripted::text("Warm.")],
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 1);
    let held = world
        .looped
        .warming
        .held()
        .expect("the step records its request");
    assert!(Arc::ptr_eq(&requests[0].conversation, &held.conversation));
}

#[test]
fn a_warming_refresh_honours_a_provider_that_never_warms() {
    struct NoWarm {
        inner: Arc<ScriptedProvider>,
    }
    impl Provider for NoWarm {
        fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
            self.inner.call(request)
        }

        fn wire_tools(
            &self,
            tools: &[contract::provider::ToolDefinition],
        ) -> Vec<Map<String, Value>> {
            self.inner.wire_tools(tools)
        }

        fn warms(&self, _: &ModelRequest) -> bool {
            false
        }
    }
    let mut world = build(Build {
        script: vec![Scripted::text("Warm.")],
        warm: Some(1),
        ..plain_build()
    });
    // The loop's provider answers `warms() == false` while sharing the
    // scripted calls: only the gate differs.
    let scripted = Arc::clone(&world.provider);
    world.looped = Loop::start(
        crate::Session {
            log: Arc::new(
                log::Log::create(
                    world.home.path(),
                    SessionId("s_cold".into()),
                    Arc::clone(&world.clock) as Arc<dyn contract::clock::Clock>,
                )
                .unwrap(),
            ),
            provider: Arc::new(NoWarm { inner: scripted }) as Arc<dyn Provider>,
            model: model("fake/model"),
            prompt: crate::PromptInputs::new(
                world.home.path().to_path_buf(),
                "/bin/sh".into(),
                world
                    .home
                    .path()
                    .join("s_cold/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&world.clock) as Arc<dyn contract::clock::Clock>,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: {
                let (tx, rx) = mpsc::channel();
                world.inbox_tx = tx;
                rx
            },
            tools: Vec::new(),
            permissions: crate::Permissions {
                workspace: world.home.path().join("workspace").display().to_string(),
                credentials: world.home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(EmptyRules),
            },
        },
        None,
    )
    .unwrap()
    .warm(Some(1));
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(
        world.looped.warm_stop(world.clock.now()),
        None,
        "a provider whose warms() is false warms nothing"
    );
}

#[test]
fn a_cancelled_turn_cancels_its_warming_refresh() {
    let mut world = build(Build {
        // One reply for the turn, one spare the cancelled refresh never reads.
        script: vec![Scripted::text("Warm."), Scripted::text("Warmer.")],
        warm: Some(1),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    world.cancel.arm();
    world.cancel.cancel();
    let start = world.clock.now();
    let stop = world.looped.warm_stop(start).expect("the session warms");
    // No code under test waits on the clock here; the refresh compares its due point only (ruling 6179406086).
    world.clock.advance(Duration::from_secs(3575));
    let refreshed = world.looped.refresh(stop).unwrap();
    assert!(
        matches!(refreshed, crate::warm::Refreshed::Stopped(_)),
        "the cancelled refresh stops warming"
    );
}

/// What the test's switcher prepares: the mode selects the factories.
#[derive(Clone, Copy, PartialEq)]
enum PrepMode {
    /// A new route with both factories.
    Switched,
    /// The new route with no factories: the old ones clear.
    Bare,
    /// A new route whose reviewer failed: the reviewer factory clears.
    FailedReviewer,
    /// A rejected preparation: nothing changes.
    Rejected,
}

#[derive(Clone)]
struct SwitchRig {
    provider_b: Arc<ScriptedProvider>,
    attempts_b: Arc<Mutex<Vec<u32>>>,
    reviewer_b: Arc<ScriptedProvider>,
    reviewer_attempts_b: Arc<Mutex<Vec<u32>>>,
    mode: Arc<Mutex<PrepMode>>,
}

fn switch_rig(script_b: Vec<Scripted>) -> SwitchRig {
    SwitchRig {
        provider_b: Arc::new(ScriptedProvider::new(script_b)),
        attempts_b: Arc::new(Mutex::new(Vec::new())),
        reviewer_b: Arc::new(ScriptedProvider::new(vec![
            Scripted::text("allow"),
            Scripted::text("allow"),
        ])),
        reviewer_attempts_b: Arc::new(Mutex::new(Vec::new())),
        mode: Arc::new(Mutex::new(PrepMode::Switched)),
    }
}

fn prepared(rig: &SwitchRig) -> crate::Prepared {
    let mode = *rig.mode.lock().unwrap();
    let reviewer: Result<Reviewer, Failure> = match mode {
        PrepMode::FailedReviewer => Err(Failure {
            code: ErrorCode::NoModel,
            message: crate::NO_MODEL_MESSAGE.into(),
            provider: None,
            retry_after_ms: None,
        }),
        PrepMode::Switched | PrepMode::Bare => Ok(Reviewer {
            provider: Arc::clone(&rig.reviewer_b) as Arc<dyn Provider>,
            model: model("fake/reviewer-2"),
            cache_lifetime: CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: Vec::new(),
        }),
        PrepMode::Rejected => unreachable!("a rejected preparation builds nothing"),
    };
    let (observe, reviewer_observe) = match mode {
        PrepMode::Switched => (
            Some(gated(&rig.attempts_b, Arc::clone(&rig.provider_b), None)),
            Some(gated(
                &rig.reviewer_attempts_b,
                Arc::clone(&rig.reviewer_b),
                None,
            )),
        ),
        // A switch to bare factories clears the old ones; a failed
        // reviewer clears only its own, keeping the session's new one.
        PrepMode::Bare => (None, None),
        PrepMode::FailedReviewer => (
            Some(gated(&rig.attempts_b, Arc::clone(&rig.provider_b), None)),
            None,
        ),
        PrepMode::Rejected => unreachable!("a rejected preparation builds nothing"),
    };
    crate::Prepared {
        provider: Arc::clone(&rig.provider_b) as Arc<dyn Provider>,
        model: model("fake/model-2"),
        thinking: None,
        chosen: None,
        credential: Some("work".into()),
        cache_lifetime: CacheLifetime::OneHour,
        context_window: fakes::CONTEXT_WINDOW,
        addendum: None,
        handoff: crate::HandoffSettings::default(),
        reviewer,
        observe,
        reviewer_observe,
        web_search: crate::Hosted::Keep,
        notice: None,
        applied: None,
        credential_files: Vec::new(),
    }
}

fn switcher(rig: &SwitchRig) -> crate::Prepare {
    let rig = rig.clone();
    Arc::new(
        move |_: &contract::commands::ModelArgs,
              _: Option<&str>,
              _: Option<contract::ThinkingLevel>| {
            if *rig.mode.lock().unwrap() == PrepMode::Rejected {
                return Err(contract::inbox::Rejection {
                    code: ErrorCode::InvalidArguments,
                    message: "no such model".into(),
                });
            }
            Ok(prepared(&rig))
        },
    )
}

fn switch(world: &mut World) {
    world
        .looped
        .take_switch(
            crate::switch::Asked::Model(contract::commands::ModelArgs {
                model: "fake/model-2".into(),
                thinking: None,
            }),
            Ack(Box::new(|_| {})),
            true,
        )
        .unwrap();
}

fn minimal_request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".into(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_test".into(),
        conversation: (Vec::new()).into(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: std::path::PathBuf::new(),
    }
}

#[test]
fn a_switch_moves_session_reviewer_and_warm_to_the_new_factories() {
    let rig = switch_rig(vec![
        tool_reply("shell", json!({})),
        Scripted::text("B-done."),
        Scripted::text("B-warm."),
    ]);
    let mut world = build(Build {
        // The capture below consumes the first entry at construction: the
        // turn's own calls start after it.
        script: vec![
            Scripted::text("Unrun."),
            tool_reply("shell", json!({})),
            Scripted::text("Done."),
        ],
        tools: vec![Arc::new(ExecTool)],
        reviewer_script: Some(vec![Scripted::text("allow")]),
        warm: Some(1),
        switch: Some(rig.clone()),
        ..plain_build()
    });
    // A call captured before the switch keeps the old route: built from
    // the old provider through its own factory, run after the switch.
    let old_vec = Arc::new(Mutex::new(Vec::new()));
    let old_factory = gated(&old_vec, Arc::clone(&world.provider), None);
    let old_requests = world.provider.requests().len();
    let before = old_factory(&minimal_request(), NonZeroU32::MIN);
    assert_eq!(
        world.provider.requests().len(),
        old_requests + 1,
        "the capture built before the switch"
    );
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 1]);
    assert_eq!(*world.reviewer_attempts.lock().unwrap(), vec![1]);
    switch(&mut world);
    let reply = before.run(&mut |_| {}).expect("the captured call runs");
    assert!(
        matches!(
            reply.actions.as_slice(),
            [contract::provider::ReplyAction::Text(text)] if text.text == "Unrun."
        ),
        "the captured call kept the old route's reply"
    );
    // The next session call and review use the new factories; the switch
    // cleared the old route's held warming request, so one new turn
    // records the new route's before its refresh.
    prompt(&world, "second");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(*rig.attempts_b.lock().unwrap(), vec![1, 1]);
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 1]);
    assert_eq!(*rig.reviewer_attempts_b.lock().unwrap(), vec![1]);
    assert_eq!(*world.reviewer_attempts.lock().unwrap(), vec![1]);
    let start = world.clock.now();
    let stop = world.looped.warm_stop(start).expect("the new route warms");
    // No code under test waits on the clock here; the refresh compares its due point only (ruling 6179406086).
    world.clock.advance(Duration::from_secs(3575));
    let refreshed = world.looped.refresh(stop).unwrap();
    assert!(matches!(refreshed, crate::warm::Refreshed::Sent));
    assert_eq!(*rig.attempts_b.lock().unwrap(), vec![1, 1, 1]);
}

#[test]
fn a_switch_to_bare_factories_clears_the_old_ones() {
    let rig = switch_rig(vec![
        tool_reply("shell", json!({})),
        Scripted::text("B-done."),
    ]);
    *rig.mode.lock().unwrap() = PrepMode::Bare;
    let mut world = build(Build {
        script: vec![tool_reply("shell", json!({})), Scripted::text("Done.")],
        tools: vec![Arc::new(ExecTool)],
        reviewer_script: Some(vec![Scripted::text("allow")]),
        switch: Some(rig.clone()),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    let old_reviewer = world.reviewer.clone().expect("a reviewer provider");
    let reviewer_requests = old_reviewer.requests().len();
    switch(&mut world);
    // The next turn calls the new providers directly: no factory builds.
    prompt(&world, "second");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert!(
        rig.attempts_b.lock().unwrap().is_empty(),
        "no session factory builds after a bare switch"
    );
    assert!(
        rig.reviewer_attempts_b.lock().unwrap().is_empty(),
        "no reviewer factory builds after a bare switch"
    );
    assert_eq!(
        rig.provider_b.requests().len(),
        2,
        "both steps reached the new provider directly"
    );
    assert_eq!(
        old_reviewer.requests().len(),
        reviewer_requests,
        "the old reviewer sees nothing more"
    );
}

#[test]
fn a_failed_reviewer_clears_only_its_factory() {
    let rig = switch_rig(vec![Scripted::text("B1."), Scripted::text("B2.")]);
    *rig.mode.lock().unwrap() = PrepMode::FailedReviewer;
    let mut world = build(Build {
        script: vec![Scripted::text("A1."), Scripted::text("A2.")],
        answerable: false,
        switch: Some(rig.clone()),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    switch(&mut world);
    // The session's new factory stands while the reviewer is gone.
    prompt(&world, "second");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(*rig.attempts_b.lock().unwrap(), vec![1]);
    assert!(
        rig.reviewer_attempts_b.lock().unwrap().is_empty(),
        "no reviewer factory builds once the reviewer failed"
    );
}

#[test]
fn a_rejected_switch_keeps_the_old_factories() {
    let rig = switch_rig(vec![Scripted::text("B1."), Scripted::text("B2.")]);
    *rig.mode.lock().unwrap() = PrepMode::Rejected;
    let mut world = build(Build {
        script: vec![Scripted::text("A1."), Scripted::text("A2.")],
        switch: Some(rig.clone()),
        ..plain_build()
    });
    prompt(&world, "hi");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    world
        .looped
        .take_switch(
            crate::switch::Asked::Model(contract::commands::ModelArgs {
                model: "fake/model-2".into(),
                thinking: None,
            }),
            Ack(Box::new(|_| {})),
            true,
        )
        .unwrap();
    // Rejected: the old factory serves the next turn, the new one idles.
    prompt(&world, "second");
    assert_eq!(turn(&mut world), Some(TurnOutcome::Completed));
    assert_eq!(*world.attempts.lock().unwrap(), vec![1, 1]);
    assert!(
        rig.attempts_b.lock().unwrap().is_empty(),
        "a rejected preparation builds nothing"
    );
}
