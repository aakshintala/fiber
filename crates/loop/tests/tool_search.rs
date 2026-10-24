//! Tool-search deferral across crates (`docs/tools.md`, "Tool search"): the
//! loop builds `tool_search` from a factory, defers the catalogue, renders
//! logged `loaded` against the preamble each request sends, and publishes
//! `/tools` rows through a setter. Every case asserts the full ordered list
//! of event kinds. The provider defers; the factory tool is test-local, as
//! is each deferring registry tool.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};

use contract::emit::Emit;
use contract::events::{CacheLifetime, TurnOutcome};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeHandoffAnswer, BeforeHandoffOutcome,
    BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolOutcome, CheckAnswer,
    CheckOutcome, ContextAnswer, ContextOutcome,
};
use contract::inbox::Delivery;
use contract::provider::{Input, ModelRequest, Provider};
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::ContentPart;
use contract::shapes::{DeclaredEffects, Effect, Failure};
use contract::tool::{Cancel, Output, Tool};
use contract::{Envelope, ErrorCode, SessionId};
use fakes::clock::FakeClock;
use fakes::{Deadline, Scripted, ScriptedProvider};
use log::{Log, Watcher};
use serde_json::{Map, Value, json};

use support::{
    assert_kinds, calls_reply, completed, delivery, model, of_kind, read_until, text_all,
};

/// A provider that defers, delegating every call to the scripted provider
/// that records the requests.
struct Deferring {
    inner: Arc<ScriptedProvider>,
}

impl Provider for Deferring {
    fn call(&self, request: &ModelRequest) -> Box<dyn contract::provider::ModelCall> {
        self.inner.call(request)
    }

    fn wire_tools(&self, tools: &[contract::provider::ToolDefinition]) -> Vec<Map<String, Value>> {
        self.inner.wire_tools(tools)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        self.inner.cost_lookup()
    }

    fn defers(&self) -> bool {
        true
    }
}

/// Wakes the waiter when the call is cancelled.
struct CancelWake(mpsc::Sender<()>);

impl contract::clock::Wake for CancelWake {
    fn wake(&self) {
        let _sent = self.0.send(());
    }
}

/// What the factory-built search tool returns, per call, in call order.
#[derive(Default)]
struct SearchScript {
    outputs: Mutex<VecDeque<Output>>,
    ran: Mutex<Vec<Map<String, Value>>>,
    /// When true the tool waits for the call's cancel before returning the
    /// scripted output, as case (m) does.
    wait_cancel: bool,
}

/// The factory-built search tool: `tool_search` with the ticket schema
/// (`query` required, `limit` defaulting to 8), scripted outputs per case.
struct SearchTool {
    script: Arc<SearchScript>,
    executes: bool,
}

impl SearchTool {
    fn output(loaded: &[&str]) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "Search output.".into(),
            }],
            loaded: Some(loaded.iter().map(|name| (*name).to_owned()).collect()),
            ..Output::default()
        }
    }
}

impl Tool for SearchTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "tool_search".into(),
            description: "Search the deferred tools.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "default": 8}
                },
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &Map<String, Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: DeclaredEffects {
                effects: vec![if self.executes {
                    Effect::Executes
                } else {
                    Effect::Reads
                }],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        self.script.ran.lock().unwrap().push(arguments.clone());
        if self.script.wait_cancel {
            // Blocks until the turn's cancel lands, as `waiting_inner`
            // does in the inner-call tests: subscribe first, then check,
            // so a cancel before the subscription is not missed.
            let (tx, rx) = mpsc::channel();
            let wake: Arc<dyn contract::clock::Wake> = Arc::new(CancelWake(tx));
            cancel.subscribe(Arc::downgrade(&wake));
            if !cancel.is_cancelled() {
                Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                    .recv(&rx)
                    .expect("the cancel landed");
            }
            assert!(cancel.is_cancelled());
        }
        self.script
            .outputs
            .lock()
            .unwrap()
            .pop_front()
            .expect("the search tool ran out of scripted outputs")
    }
}

/// A deferring registry tool with a preset output: the catalogue the
/// factory builds the search from.
struct CatalogTool {
    name: &'static str,
    deferred: bool,
    output: Output,
}

impl CatalogTool {
    fn deferred(name: &'static str) -> Self {
        Self {
            name,
            deferred: true,
            output: Output {
                content: vec![ContentPart::Text {
                    text: format!("{name} output."),
                }],
                ..Output::default()
            },
        }
    }

    /// A tool whose result carries the handoff `note`, ending the context.
    fn wrapping(name: &'static str, note: &str) -> Self {
        let mut tool = Self::deferred(name);
        tool.deferred = false;
        tool.output.control = Some(contract::events::Control {
            handoff: Some(note.into()),
            ..Default::default()
        });
        tool
    }
}

impl Tool for CatalogTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: self.name.into(),
            description: format!("The test tool {}.", self.name),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
                "additionalProperties": false
            }),
            deferred: self.deferred,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &Map<String, Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.output.clone()
    }
}

/// The `/tools` rows the loop publishes, in publish order.
#[derive(Default)]
struct Rows {
    declare: Mutex<Vec<(String, Option<contract::events::ToolInfo>)>>,
    state: Mutex<Vec<(String, contract::events::ToolState)>>,
}

impl Rows {
    fn setter(rows: &Arc<Rows>) -> r#loop::ToolRows {
        let declare_rows = Arc::clone(rows);
        let state_rows = Arc::clone(rows);
        r#loop::ToolRows {
            declare: Arc::new(move |name, info| {
                declare_rows
                    .declare
                    .lock()
                    .unwrap()
                    .push((name.to_owned(), info));
            }),
            state: Arc::new(move |name, state| {
                state_rows
                    .state
                    .lock()
                    .unwrap()
                    .push((name.to_owned(), state));
            }),
        }
    }

    fn declared(&self, name: &str) -> usize {
        self.declare
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, info)| seen == name && info.is_some())
            .count()
    }

    fn removed(&self, name: &str) -> usize {
        self.declare
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, info)| seen == name && info.is_none())
            .count()
    }

    fn states(&self, name: &str) -> Vec<contract::events::ToolState> {
        self.state
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| seen == name)
            .map(|(_, state)| *state)
            .collect()
    }
}

/// Hooks answering `after_tool` from a queue, passing everything else
/// through: case (f) rewrites content, case (g) withholds the result.
struct AfterToolHooks {
    answers: Mutex<VecDeque<AfterToolAnswer>>,
}

impl AfterToolHooks {
    fn queued(answers: Vec<AfterToolAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into()),
        })
    }

    fn changed(text: &str) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Changed {
                content: Some(text.into()),
                details: None,
                artifact: None,
            },
            changed_by: vec!["ext".into()],
            notices: Vec::new(),
        }
    }

    fn withheld() -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Withheld {
                extension: "ext".into(),
            },
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }
}

impl contract::hook::Hooks for AfterToolHooks {
    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(AfterToolAnswer {
                outcome: AfterToolOutcome::Unchanged,
                changed_by: Vec::new(),
                notices: Vec::new(),
            })
    }

    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, _: &contract::hook::BeforeMessage<'_>) -> BeforeMessageAnswer {
        BeforeMessageAnswer {
            outcome: BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(&self, _: &contract::hook::BeforeModelCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> ContextAnswer {
        ContextAnswer {
            outcome: ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(&self, _: &contract::hook::BeforeHandoff<'_>) -> BeforeHandoffAnswer {
        BeforeHandoffAnswer {
            outcome: BeforeHandoffOutcome::NoNote,
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

/// One session on the deferring provider: its scripted provider, log,
/// inbox and loop, with the factory and rows wired.
struct Harness {
    dir: std::path::PathBuf,
    log: Arc<Log>,
    clock: Arc<FakeClock>,
    scripted: Arc<ScriptedProvider>,
    inbox: mpsc::Sender<Delivery>,
    looped: Option<r#loop::Loop>,
    cancel: Arc<r#loop::TurnCancel>,
    workspace: std::path::PathBuf,
    credentials: std::path::PathBuf,
    rules: Arc<support::FakeRules>,
    lines: Option<Watcher>,
    rows: Arc<Rows>,
    search: Arc<SearchScript>,
    executes: bool,
    _home: fakes::TempDir,
}

impl Harness {
    /// Starts a session on `script` with `tools`, the factory building the
    /// search tool from `script`'s outputs, and `hooks` when set.
    fn start(
        script: Vec<Scripted>,
        tools: Vec<(String, Arc<dyn Tool>)>,
        search: Arc<SearchScript>,
        executes: bool,
        hooks: Option<Arc<dyn contract::hook::Hooks>>,
    ) -> Self {
        let home = fakes::TempDir::new("fiber-tool-search");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let clock = FakeClock::new();
        let log =
            Arc::new(Log::create(home.path(), SessionId("s_test".into()), clock.clone()).unwrap());
        let lines = Some(log.watch());
        let scripted = Arc::new(ScriptedProvider::new(script));
        let provider: Arc<dyn Provider> = Arc::new(Deferring {
            inner: Arc::clone(&scripted),
        });
        let (inbox, rx) = mpsc::channel();
        let dir = home.path().join("s_test");
        let prompt_clock: Arc<dyn contract::clock::Clock> = Arc::clone(&clock) as _;
        let rules = Arc::new(support::FakeRules::empty());
        let search_tool = Arc::clone(&search);
        let factory: r#loop::ToolSearchFactory = Arc::new(move |_, _| {
            Arc::new(SearchTool {
                script: Arc::clone(&search_tool),
                executes,
            }) as Arc<dyn Tool>
        });
        let rows = Arc::new(Rows::default());
        let mut looped = r#loop::Loop::start(
            r#loop::Session {
                log: Arc::clone(&log),
                provider,
                model: r#loop::Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: {
                    let mut prompt = r#loop::PromptInputs::new(
                        home.path().to_path_buf(),
                        "/bin/sh".into(),
                        dir.join("events.jsonl").display().to_string(),
                        prompt_clock,
                        fakes::CONTEXT_WINDOW,
                    );
                    prompt.credential = Some("work".into());
                    prompt
                },
                inbox: rx,
                tools,
                permissions: r#loop::Permissions {
                    workspace: workspace.display().to_string(),
                    credentials: credentials.clone(),
                    credential_files: Vec::new(),
                    rules: Arc::clone(&rules) as _,
                },
            },
            None,
        )
        .unwrap()
        .tool_search(factory)
        .tool_rows(Rows::setter(&rows));
        if let Some(hooks) = hooks {
            looped = looped.hooks(hooks);
        }
        let cancel = Arc::new(r#loop::TurnCancel::default());
        looped = looped.cancelled_by(Arc::clone(&cancel));
        Self {
            dir,
            log,
            clock,
            scripted,
            inbox,
            looped: Some(looped),
            cancel,
            workspace,
            credentials,
            rules,
            lines,
            rows,
            search,
            executes,
            _home: home,
        }
    }

    /// Resumes the session's log in a new loop with `tools`, as a new
    /// process does: the deferring provider, factory and rows are wired
    /// again, so the resumed request renders against the rebuilt preamble.
    fn resume(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) {
        let folded = r#loop::resumed(&self.dir).unwrap();
        r#loop::fiber_started(&self.log, "0.0.1", true).unwrap();
        let (inbox, rx) = mpsc::channel();
        self.inbox = inbox;
        self.lines = Some(self.log.watch());
        let home = self.dir.parent().unwrap().to_path_buf();
        let prompt_clock: Arc<dyn contract::clock::Clock> = Arc::clone(&self.clock) as _;
        let mut prompt = r#loop::PromptInputs::new(
            home,
            "/bin/sh".into(),
            self.dir.join("events.jsonl").display().to_string(),
            prompt_clock,
            fakes::CONTEXT_WINDOW,
        );
        prompt.credential = Some("work".into());
        let search = Arc::clone(&self.search);
        let executes = self.executes;
        let factory: r#loop::ToolSearchFactory = Arc::new(move |_, _| {
            Arc::new(SearchTool {
                script: Arc::clone(&search),
                executes,
            }) as Arc<dyn Tool>
        });
        self.cancel = Arc::new(r#loop::TurnCancel::default());
        let looped = r#loop::Loop::resume(
            r#loop::Session {
                log: Arc::clone(&self.log),
                provider: Arc::new(Deferring {
                    inner: Arc::clone(&self.scripted),
                }) as Arc<dyn Provider>,
                model: r#loop::Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt,
                inbox: rx,
                tools,
                permissions: r#loop::Permissions {
                    workspace: self.workspace.display().to_string(),
                    credentials: self.credentials.clone(),
                    credential_files: Vec::new(),
                    rules: Arc::clone(&self.rules) as _,
                },
            },
            folded,
        )
        .unwrap()
        .tool_search(factory)
        .tool_rows(Rows::setter(&self.rows))
        .cancelled_by(Arc::clone(&self.cancel));
        self.looped = Some(looped);
    }

    /// Sends `prompt` and runs one turn, failing at [`fakes::MUST_SUCCEED_WITHIN`].
    fn turn(&mut self, prompt: &str) -> Option<TurnOutcome> {
        self.inbox.send(delivery(prompt)).unwrap();
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&finished)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    /// Every request the scripted provider recorded, in order.
    fn requests(&self) -> Vec<ModelRequest> {
        self.scripted.requests()
    }

    /// Every line emitted since the last call, through `turn_completed`.
    fn lines(&mut self) -> Vec<Envelope> {
        let watcher = self.lines.take().unwrap();
        let (watcher, lines) = read_until(watcher, "a turn_completed line", |line| {
            line.kind == "turn_completed"
        });
        self.lines = Some(watcher);
        lines
            .into_iter()
            .filter(|line| line.kind != "session_status")
            .collect()
    }
}

/// The kinds of a first turn whose reply calls once with no text and whose
/// continuation says "Done.": `middle` stands in for the decision and
/// completion lines.
fn kinds_with(middle: &[&'static str]) -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    kinds.extend(middle.iter().copied());
    kinds.extend(
        [
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
        .iter()
        .copied(),
    );
    kinds
}

fn search_arguments(query: &str) -> Value {
    json!({"query": query})
}

/// Whether the input is the session's opening environment message.
fn is_opening(input: &Input) -> bool {
    matches!(input, Input::User { text, .. } if text.starts_with("This message is from Fiber"))
}

fn preamble_of(lines: &[Envelope]) -> &Envelope {
    of_kind(lines, "preamble_built")[0]
}

fn deferred_names(lines: &[Envelope]) -> Vec<String> {
    preamble_of(lines).payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["deferred"].as_bool().unwrap())
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

fn declared_names(lines: &[Envelope]) -> Vec<String> {
    preamble_of(lines).payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_search_call_completes_loaded_and_the_next_request_carries_it_natively() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "completed");
    assert_eq!(done[0].payload["loaded"], json!(["a"]));
    assert_eq!(deferred_names(&lines), ["a"]);
    assert_eq!(declared_names(&lines), ["a", "tool_search"]);

    let requests = harness.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].conversation.len(), 4);
    assert!(is_opening(&requests[1].conversation[0]));
    assert!(matches!(&requests[1].conversation[1], Input::User { text, .. } if text == "hi"));
    let Input::ToolCall { call, .. } = &requests[1].conversation[2] else {
        panic!("expected a ToolCall, got {:?}", requests[1].conversation[2])
    };
    assert_eq!(call.name, "tool_search");
    assert_eq!(call.arguments, search_arguments("a"));
    assert_eq!(
        requests[1].conversation[3],
        Input::ToolSearchResult {
            action_id: done[0].action_id.clone().unwrap(),
            loaded: vec!["a".into()],
        }
    );
}

#[test]
fn two_searches_in_one_step_render_at_their_own_positions() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"]), SearchTool::output(&["b"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply(
                "",
                &[
                    ("tool_search", search_arguments("a")),
                    ("tool_search", search_arguments("b")),
                ],
            ),
            Scripted::text("Done."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("b"))),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    assert_eq!(deferred_names(&lines), ["a", "b"]);

    // Both results render natively, each at its own position: no dedupe.
    let requests = harness.requests();
    assert_eq!(requests.len(), 2);
    let conversation = &requests[1].conversation;
    assert_eq!(conversation.len(), 6);
    let mut loaded: Vec<Vec<String>> = Vec::new();
    for input in &conversation[4..] {
        let Input::ToolSearchResult { loaded: names, .. } = input else {
            panic!("expected a ToolSearchResult, got {input:?}")
        };
        loaded.push(names.clone());
    }
    loaded.sort();
    assert_eq!(loaded, [vec!["a".to_owned()], vec!["b".to_owned()]]);
}

#[test]
fn a_handoff_after_a_search_leaves_no_search_result_for_the_next_request() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            calls_reply("", &[("wrap", json!({"city": "Paris"}))]),
            Scripted::text("Done."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            (
                "fx".to_owned(),
                Arc::new(CatalogTool::wrapping("wrap", "Tool note.")),
            ),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    // The first turn loads `a` through the search.
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let first = harness.lines();
    assert_kinds(
        &first,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    // The second turn hands off on `wrap` with no search in the step: the
    // handoff drops the earlier search with the turn's conversation.
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "handoff_completed",
            "opening_message",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    // Four requests: each turn's own request and its continuation. The
    // restart carries the note but no search result: the earlier loaded
    // search left the conversation with the handoff.
    let requests = harness.requests();
    assert_eq!(requests.len(), 4);
    let restart = &requests[3].conversation;
    assert!(
        restart
            .iter()
            .any(|input| matches!(input, Input::User { text, .. } if text == "Tool note.")),
        "the restart carries the note: {restart:?}"
    );
    assert!(
        !restart
            .iter()
            .any(|input| matches!(input, Input::ToolSearchResult { .. })),
        "no search result survives the handoff"
    );
    assert!(
        !restart
            .iter()
            .any(|input| matches!(input, Input::ToolCall { .. })),
        "no search call survives the handoff"
    );
    // The earlier load moved back to deferred: declared, loaded, then
    // dropped by the handoff. The second turn reuses the first build, so
    // `a` is declared once.
    assert_eq!(harness.rows.declared("a"), 1);
    let states = harness.rows.states("a");
    assert!(states.contains(&contract::events::ToolState::Loaded));
    assert_eq!(states.last(), Some(&contract::events::ToolState::Deferred));
}

#[test]
fn bad_search_arguments_fail_before_the_call_starts() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply(
                "",
                &[
                    ("tool_search", json!({"query": ""})),
                    ("tool_search", json!({"query": "x", "limit": 0})),
                    ("tool_search", json!({"query": "x", "limit": 1})),
                ],
            ),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    // Two schema failures complete without starting; the third call runs.
    assert_kinds(
        &lines,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 3);
    for failed in &done[..2] {
        assert_eq!(failed.payload["status"], "failed");
        assert_eq!(failed.payload["error"]["code"], "invalid_arguments");
    }
    assert_eq!(done[2].payload["status"], "completed");
    assert_eq!(done[2].payload["loaded"], json!(["a"]));
}

#[test]
fn a_rewritten_search_keeps_its_loaded_set_and_native_form() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let hooks = AfterToolHooks::queued(vec![AfterToolHooks::changed("Hook text.")]);
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        Some(hooks as Arc<dyn contract::hook::Hooks>),
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    // The hook's text wins in the log, while the loaded set survives.
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(text_all(done[0]), "Hook text.");
    assert_eq!(done[0].payload["loaded"], json!(["a"]));
    assert_eq!(done[0].payload["changed_by"], json!(["ext"]));
    // The rendered form is unchanged: still native.
    let requests = harness.requests();
    assert_eq!(
        requests[1].conversation[3],
        Input::ToolSearchResult {
            action_id: done[0].action_id.clone().unwrap(),
            loaded: vec!["a".into()],
        }
    );
    assert!(
        harness
            .rows
            .states("a")
            .contains(&contract::events::ToolState::Loaded)
    );
}

#[test]
fn a_withheld_search_renders_as_a_plain_result() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let hooks = AfterToolHooks::queued(vec![AfterToolHooks::withheld()]);
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        Some(hooks as Arc<dyn contract::hook::Hooks>),
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert!(done[0].payload.get("loaded").is_none());
    let withheld = "Output withheld: the `after_tool` hook of extension ext failed.";
    assert_eq!(text_all(done[0]), withheld);
    let requests = harness.requests();
    let result = requests[1]
        .conversation
        .iter()
        .find(|input| matches!(input, Input::ToolResult { .. }))
        .expect("one plain result");
    let Input::ToolResult { text, is_error, .. } = result else {
        panic!("expected a plain ToolResult, got {result:?}")
    };
    assert_eq!(text, withheld);
    assert!(!is_error);
}

#[test]
fn a_failed_search_renders_as_an_ordinary_error_result() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(
            vec![Output {
                content: vec![ContentPart::Text {
                    text: "Search broke.".into(),
                }],
                error: Some(Failure {
                    code: ErrorCode::ToolError,
                    message: "Search broke.".into(),
                    provider: None,
                    retry_after_ms: None,
                }),
                ..Output::default()
            }]
            .into(),
        ),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "failed");
    assert!(done[0].payload.get("loaded").is_none());
    let requests = harness.requests();
    let result = requests[1]
        .conversation
        .iter()
        .find(|input| matches!(input, Input::ToolResult { .. }))
        .expect("one error result");
    let Input::ToolResult { text, is_error, .. } = result else {
        panic!("expected an error ToolResult, got {result:?}")
    };
    assert_eq!(text, "Search broke.");
    assert!(is_error);
}

#[test]
fn a_handoff_carries_a_same_step_search_past_the_note() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply(
                "",
                &[
                    ("tool_search", search_arguments("a")),
                    ("wrap", json!({"city": "Paris"})),
                ],
            ),
            Scripted::text("Done."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            (
                "fx".to_owned(),
                Arc::new(CatalogTool::wrapping("wrap", "Wrap note.")),
            ),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "handoff_completed",
            "opening_message",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    // The restart carries the search call and its result after the note.
    let requests = harness.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].conversation.len(), 5);
    assert!(is_opening(&requests[1].conversation[0]));
    assert!(matches!(&requests[1].conversation[1], Input::User { text, .. } if text == "hi"));
    assert!(
        matches!(&requests[1].conversation[2], Input::User { text, .. } if text == "Wrap note.")
    );
    let Input::ToolCall { call, .. } = &requests[1].conversation[3] else {
        panic!(
            "expected the search call, got {:?}",
            requests[1].conversation[3]
        )
    };
    assert_eq!(call.name, "tool_search");
    assert_eq!(
        requests[1].conversation[4],
        Input::ToolSearchResult {
            action_id: done[0].action_id.clone().unwrap(),
            loaded: vec!["a".into()],
        }
    );
    assert!(
        harness
            .rows
            .states("a")
            .contains(&contract::events::ToolState::Loaded)
    );
    // The carried result survives the handoff, so `a` stays loaded.
    assert_eq!(
        harness.rows.states("a").last(),
        Some(&contract::events::ToolState::Loaded)
    );
}

#[test]
fn rows_follow_declared_states_and_search_loads_only_deferred_names() {
    const OTHER: &str = "fake/other";
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a", "b"])].into()),
        ..SearchScript::default()
    });
    let second = Arc::new(ScriptedProvider::new(vec![Scripted::text("Again.")]));
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            (
                "fx".to_owned(),
                Arc::new(CatalogTool {
                    name: "c",
                    deferred: false,
                    output: Output {
                        content: vec![ContentPart::Text {
                            text: "c output.".into(),
                        }],
                        ..Output::default()
                    },
                }),
            ),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    // A second deferring provider behind the switch: the next turn rebuilds
    // the same preamble and sends every row again.
    use contract::commands::ModelArgs;
    let deferring = Arc::clone(&harness.scripted);
    let plain = Arc::clone(&second);
    let prepare: r#loop::Prepare = Arc::new(
        move |args: &ModelArgs, _: Option<&str>, chosen: Option<contract::ThinkingLevel>| {
            let provider: Arc<dyn Provider> = if args.model == OTHER {
                Arc::new(Deferring {
                    inner: Arc::clone(&plain),
                }) as Arc<dyn Provider>
            } else {
                Arc::new(Deferring {
                    inner: Arc::clone(&deferring),
                }) as Arc<dyn Provider>
            };
            Ok(r#loop::Prepared {
                provider,
                model: r#loop::Model {
                    reference: args.model.clone(),
                    cost: None,
                    subscription: false,
                },
                thinking: None,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: r#loop::HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: r#loop::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
                observe: None,
                reviewer_observe: None,
            })
        },
    );
    harness.looped = harness
        .looped
        .take()
        .map(|looped| looped.switcher(prepare, r#loop::Switchable { chosen: None }));
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    harness.inbox.send(model(OTHER, None)).unwrap();
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let rows = &harness.rows;
    // After two builds every declared tool's row was sent twice; `a`
    // also published its load in between.
    for name in ["a", "c", "tool_search"] {
        assert_eq!(rows.declared(name), 2, "declare sent twice for {name}");
    }
    assert_eq!(rows.states("c").len(), 2);
    assert_eq!(rows.states("tool_search").len(), 2);
    // The factory-inserted search declares as builtin, not as an
    // extension row.
    let search_info = rows
        .declare
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(seen, _)| seen == "tool_search")
        .and_then(|(_, info)| info.clone());
    assert!(
        matches!(
            search_info,
            Some(contract::events::ToolInfo {
                source: contract::events::ToolSource::Builtin,
                ..
            })
        ),
        "the search row declares a builtin source: {search_info:?}"
    );
    assert_eq!(
        rows.states("a"),
        vec![
            contract::events::ToolState::Deferred,
            contract::events::ToolState::Loaded,
            contract::events::ToolState::Deferred,
        ]
    );
    assert_eq!(
        rows.states("c"),
        vec![
            contract::events::ToolState::Full,
            contract::events::ToolState::Full,
        ]
    );
    // The search loaded `a` and `b`, but only the deferred name shows loaded.
    assert!(
        !rows
            .states("b")
            .contains(&contract::events::ToolState::Loaded)
    );
}

#[test]
fn a_denied_search_renders_as_a_plain_result() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        true,
        None,
    );
    harness.rules.set(StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Deny,
            tool: "tool_search".into(),
            prefix: String::new(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    });
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&kinds_with(&["permission_resolved", "tool_call_completed"])],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "denied");
    assert!(done[0].payload.get("loaded").is_none());
    let text = "A standing rule refuses this call. It did not run.";
    assert_eq!(text_all(done[0]), text);
    let requests = harness.requests();
    let result = requests[1]
        .conversation
        .iter()
        .find(|input| matches!(input, Input::ToolResult { .. }))
        .expect("one plain result");
    let Input::ToolResult {
        text: seen,
        is_error,
        ..
    } = result
    else {
        panic!("expected a plain ToolResult, got {result:?}")
    };
    assert_eq!(seen, text);
    assert!(!is_error);
    assert!(
        !harness
            .rows
            .states("a")
            .contains(&contract::events::ToolState::Loaded)
    );
}

#[test]
fn a_resumed_session_renders_only_names_it_still_defers() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a", "b"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            calls_reply("", &[("b", json!({"city": "Paris"}))]),
            Scripted::text("Done again."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("b"))),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["loaded"], json!(["a", "b"]));
    let searched = done[0].action_id.clone().unwrap();

    // Resume with `b` gone from the registry: the preamble defers only
    // `a`, so the logged list renders as `[a]`, and a call to `b` fails
    // `unknown_tool`.
    harness.resume(vec![(
        "fx".to_owned(),
        Arc::new(CatalogTool::deferred("a")),
    )]);
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    assert_eq!(deferred_names(&lines), ["a"]);
    assert_eq!(declared_names(&lines), ["a", "tool_search"]);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "unknown_tool");

    let requests = harness.requests();
    let resumed = requests.last().unwrap();
    let rendered: Vec<&Input> = resumed
        .conversation
        .iter()
        .filter(|input| matches!(input, Input::ToolSearchResult { .. }))
        .collect();
    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0],
        &Input::ToolSearchResult {
            action_id: searched,
            loaded: vec!["a".into()],
        }
    );
    // The resumed conversation never names `b` as loaded.
    assert!(
        !resumed.conversation.iter().any(|input| matches!(input,
            Input::ToolSearchResult { loaded, .. } if loaded.contains(&"b".to_owned()))),
        "the missing name is dropped from the block"
    );
}

#[test]
fn a_rewound_session_matches_the_parent_request_bytes() {
    use contract::Seq;
    use contract::shapes::Point;

    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            Scripted::text("Again."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let _ = harness.lines();
    assert_eq!(harness.turn("two"), Some(TurnOutcome::Completed));
    let a_lines = harness.lines();
    let a_requests = harness.requests();
    assert_eq!(a_requests.len(), 3);
    // Turn 2's input is the last input of its first request: everything
    // before it is the history the rewind keeps.
    let a_second = &a_requests[2];
    let kept = a_second.conversation.len() - 1;

    let at = a_lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0)
        .next()
        .unwrap()
        - 1;
    let note = r#loop::rewind_note(&harness.dir, Seq(at)).unwrap();

    // B rewinds to turn 2's start with the same registry: its first
    // request renders the logged search against the logged preamble's
    // deferred set, byte for byte.
    let home = harness.dir.parent().unwrap().to_path_buf();
    let clock = Arc::clone(&harness.clock);
    let log = Arc::new(Log::create(&home, SessionId("s_rewound".into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let b_search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let b_scripted = Arc::new(ScriptedProvider::new(vec![
        calls_reply("", &[("tool_search", search_arguments("a"))]),
        Scripted::text("B done."),
    ]));
    let (tx, rx) = mpsc::channel();
    let prompt_clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        prompt_clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let b_search_tool = Arc::clone(&b_search);
    let factory: r#loop::ToolSearchFactory = Arc::new(move |_, _| {
        Arc::new(SearchTool {
            script: Arc::clone(&b_search_tool),
            executes: false,
        }) as Arc<dyn Tool>
    });
    let b_log = Arc::clone(&log);
    let mut b = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: Arc::new(Deferring {
                inner: b_scripted.clone(),
            }) as Arc<dyn Provider>,
            model: r#loop::Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![(
                "fx".to_owned(),
                Arc::new(CatalogTool::deferred("a")) as Arc<dyn Tool>,
            )],
            permissions: r#loop::Permissions {
                workspace: harness.workspace.display().to_string(),
                credentials: harness.credentials.clone(),
                credential_files: Vec::new(),
                rules: Arc::clone(&harness.rules) as _,
            },
        },
        r#loop::Rewound {
            factory: Some(factory),
            from: Point {
                session_id: SessionId("s_test".into()),
                seq: Seq(at),
            },
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note,
                summary: None,
                name: r#loop::latest_name(&harness.dir),
            },
        },
    )
    .unwrap();
    tx.send(delivery("three")).unwrap();
    let watcher = b_log.watch();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (_b, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let (_, b_lines) = read_until(watcher, "a turn_completed line", |line| {
        line.kind == "turn_completed"
    });

    let b_requests = b_scripted.requests();
    assert_eq!(b_requests.len(), 2);
    let first = &b_requests[0];
    // The first request matches the parent's turn-2 request up to its new
    // input: same system prompt, tools, and rendered history.
    assert_eq!(first.system_prompt, a_second.system_prompt);
    assert_eq!(first.tools, a_second.tools);
    assert_eq!(&first.conversation[..kept], &a_second.conversation[..kept]);
    assert!(matches!(
        &first.conversation[3],
        Input::ToolSearchResult { loaded, .. } if loaded == &vec!["a".to_owned()]
    ));
    // The registry holds the search tool the factory built from the
    // logged D: the rewound turn's search call completes loaded.
    assert_eq!(b_search.ran.lock().unwrap().len(), 1);
    let b_done = completed(&b_lines);
    assert_eq!(b_done.len(), 1);
    assert_eq!(b_done[0].payload["loaded"], json!(["a"]));
}

#[test]
fn a_rewound_session_orders_the_rebuilt_search_and_reports_its_inputs() {
    use contract::Seq;
    use contract::shapes::Point;

    fn full_zebra() -> CatalogTool {
        CatalogTool {
            name: "zebra",
            deferred: false,
            output: Output {
                content: vec![ContentPart::Text {
                    text: "zebra output.".into(),
                }],
                ..Output::default()
            },
        }
    }

    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            Scripted::text("Again."),
        ],
        vec![
            ("fx".to_owned(), Arc::new(CatalogTool::deferred("a"))),
            ("fy".to_owned(), Arc::new(full_zebra())),
        ],
        Arc::clone(&search),
        false,
        None,
    );
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let first_lines = harness.lines();
    assert_kinds(
        &first_lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    // A registered name sorting after the search name: the build merges
    // the search ahead of it.
    assert_eq!(deferred_names(&first_lines), ["a"]);
    assert_eq!(declared_names(&first_lines), ["a", "tool_search", "zebra"]);
    assert_eq!(harness.turn("two"), Some(TurnOutcome::Completed));
    let a_lines = harness.lines();
    assert_kinds(
        &a_lines,
        &[&[
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    let a_requests = harness.requests();
    assert_eq!(a_requests.len(), 3);
    // Turn 2's input is the last input of its first request: everything
    // before it is the history the rewind keeps.
    let a_second = &a_requests[2];
    let kept = a_second.conversation.len() - 1;

    let at = a_lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0)
        .next()
        .unwrap()
        - 1;
    let note = r#loop::rewind_note(&harness.dir, Seq(at)).unwrap();

    // B rewinds to turn 2's start with the same registry: its first
    // request sends the parent's tools byte for byte, and the factory
    // sees the logged-D catalogue and sources.
    let home = harness.dir.parent().unwrap().to_path_buf();
    let clock = Arc::clone(&harness.clock);
    let log = Arc::new(Log::create(&home, SessionId("s_rewound".into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let b_search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let b_scripted = Arc::new(ScriptedProvider::new(vec![
        calls_reply("", &[("tool_search", search_arguments("a"))]),
        Scripted::text("B done."),
    ]));
    let (tx, rx) = mpsc::channel();
    let prompt_clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        prompt_clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_factory = Arc::clone(&seen);
    let b_search_tool = Arc::clone(&b_search);
    let factory: r#loop::ToolSearchFactory = Arc::new(move |catalogue, sources| {
        seen_factory.lock().unwrap().push((
            catalogue
                .iter()
                .map(|definition| definition.name.clone())
                .collect::<Vec<_>>(),
            sources.to_vec(),
        ));
        Arc::new(SearchTool {
            script: Arc::clone(&b_search_tool),
            executes: false,
        }) as Arc<dyn Tool>
    });
    let b_log = Arc::clone(&log);
    let mut b = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: Arc::new(Deferring {
                inner: b_scripted.clone(),
            }) as Arc<dyn Provider>,
            model: r#loop::Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![
                (
                    "fx".to_owned(),
                    Arc::new(CatalogTool::deferred("a")) as Arc<dyn Tool>,
                ),
                ("fy".to_owned(), Arc::new(full_zebra()) as Arc<dyn Tool>),
            ],
            permissions: r#loop::Permissions {
                workspace: harness.workspace.display().to_string(),
                credentials: harness.credentials.clone(),
                credential_files: Vec::new(),
                rules: Arc::clone(&harness.rules) as _,
            },
        },
        r#loop::Rewound {
            factory: Some(factory),
            from: Point {
                session_id: SessionId("s_test".into()),
                seq: Seq(at),
            },
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note,
                summary: None,
                name: r#loop::latest_name(&harness.dir),
            },
        },
    )
    .unwrap();
    tx.send(delivery("three")).unwrap();
    let watcher = b_log.watch();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (_b, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let (_, b_lines) = read_until(watcher, "a turn_completed line", |line| {
        line.kind == "turn_completed"
    });
    assert_kinds(
        &b_lines,
        &[&[
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );

    let b_requests = b_scripted.requests();
    assert_eq!(b_requests.len(), 2);
    let first = &b_requests[0];
    // The first request matches the parent's turn-2 request up to its new
    // input: the rebuilt search merges into the same position, so the
    // declared tools match byte for byte.
    assert_eq!(first.system_prompt, a_second.system_prompt);
    assert_eq!(first.tools, a_second.tools);
    assert_eq!(&first.conversation[..kept], &a_second.conversation[..kept]);
    assert!(matches!(
        &first.conversation[3],
        Input::ToolSearchResult { loaded, .. } if loaded == &vec!["a".to_owned()]
    ));
    // The factory saw the logged-D catalogue and sources: the deferred
    // definition, and every deferred source but builtin.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, ["a"]);
    assert_eq!(seen[0].1, ["fx"]);
    // The registry holds the search tool the factory built from the
    // logged D: the rewound turn's search call completes loaded.
    assert_eq!(b_search.ran.lock().unwrap().len(), 1);
    let b_done = completed(&b_lines);
    assert_eq!(b_done.len(), 1);
    assert_eq!(b_done[0].payload["loaded"], json!(["a"]));
}

/// A rewind onto a non-deferring provider renders the logged search as
/// text, as the live loop does: the logged D is dropped, so the first
/// request carries `Loaded: a.`, declares no `tool_search`, and the
/// registry holds none.
#[test]
fn a_rewound_session_off_deferral_renders_the_logged_search_as_text() {
    use contract::Seq;
    use contract::commands::ModelArgs;
    use contract::shapes::Point;

    const PLAIN: &str = "fake/plain";
    const DEFERRING: &str = "fake/model";
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let plain_scripted = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("Plain."),
        Scripted::text("B done."),
    ]));
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    let deferring = Arc::clone(&harness.scripted);
    let plain = Arc::clone(&plain_scripted);
    let prepare: r#loop::Prepare = Arc::new(
        move |args: &ModelArgs, _: Option<&str>, chosen: Option<contract::ThinkingLevel>| {
            let provider: Arc<dyn Provider> = if args.model == DEFERRING {
                Arc::new(Deferring {
                    inner: Arc::clone(&deferring),
                }) as Arc<dyn Provider>
            } else {
                Arc::clone(&plain) as Arc<dyn Provider>
            };
            Ok(r#loop::Prepared {
                provider,
                model: r#loop::Model {
                    reference: args.model.clone(),
                    cost: None,
                    subscription: false,
                },
                thinking: None,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: r#loop::HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: r#loop::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
                observe: None,
                reviewer_observe: None,
            })
        },
    );
    harness.looped = harness
        .looped
        .take()
        .map(|looped| looped.switcher(prepare, r#loop::Switchable { chosen: None }));

    // Turn 1 defers: the search loads `a`.
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let first_lines = harness.lines();
    assert_kinds(
        &first_lines,
        &[&kinds_with(&["tool_call_started", "tool_call_completed"])],
    );
    assert_eq!(deferred_names(&first_lines), ["a"]);

    // The switch rebuilds without deferral: the parent renders the
    // logged search as text.
    harness.inbox.send(model(PLAIN, None)).unwrap();
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let a_lines = harness.lines();
    assert_kinds(
        &a_lines,
        &[&[
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    assert_eq!(deferred_names(&a_lines), Vec::<String>::new());
    assert_eq!(declared_names(&a_lines), ["a"]);
    let a_requests = plain_scripted.requests();
    assert_eq!(a_requests.len(), 1);
    assert!(
        a_requests[0].conversation.iter().any(|input| matches!(
            input,
            Input::ToolResult { text, is_error, .. } if text == "Loaded: a." && !is_error
        )),
        "the parent renders the search as text: {:?}",
        a_requests[0].conversation
    );

    let at = a_lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0)
        .next()
        .unwrap()
        - 1;
    let note = r#loop::rewind_note(&harness.dir, Seq(at)).unwrap();

    // B rewinds to turn 2's start with a factory and a plain provider:
    // the logged D is dropped, so the first request renders text as the
    // live loop does, not the zero-match text.
    let home = harness.dir.parent().unwrap().to_path_buf();
    let clock = Arc::clone(&harness.clock);
    let log = Arc::new(Log::create(&home, SessionId("s_rewound".into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let b_search = Arc::new(SearchScript::default());
    let (tx, rx) = mpsc::channel();
    let prompt_clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = r#loop::PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        prompt_clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_factory = Arc::clone(&seen);
    let b_search_tool = Arc::clone(&b_search);
    let factory: r#loop::ToolSearchFactory = Arc::new(move |catalogue, sources| {
        seen_factory.lock().unwrap().push((
            catalogue
                .iter()
                .map(|definition| definition.name.clone())
                .collect::<Vec<_>>(),
            sources.to_vec(),
        ));
        Arc::new(SearchTool {
            script: Arc::clone(&b_search_tool),
            executes: false,
        }) as Arc<dyn Tool>
    });
    let b_log = Arc::clone(&log);
    let mut b = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: Arc::clone(&plain_scripted) as Arc<dyn Provider>,
            model: r#loop::Model {
                reference: "fake/plain".into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![(
                "fx".to_owned(),
                Arc::new(CatalogTool::deferred("a")) as Arc<dyn Tool>,
            )],
            permissions: r#loop::Permissions {
                workspace: harness.workspace.display().to_string(),
                credentials: harness.credentials.clone(),
                credential_files: Vec::new(),
                rules: Arc::clone(&harness.rules) as _,
            },
        },
        r#loop::Rewound {
            factory: Some(factory),
            from: Point {
                session_id: SessionId("s_test".into()),
                seq: Seq(at),
            },
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note,
                summary: None,
                name: r#loop::latest_name(&harness.dir),
            },
        },
    )
    .unwrap();
    tx.send(delivery("three")).unwrap();
    let watcher = b_log.watch();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (_b, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let (_, b_lines) = read_until(watcher, "a turn_completed line", |line| {
        line.kind == "turn_completed"
    });
    assert_kinds(
        &b_lines,
        &[&[
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    // The factory never built: the registry holds no search tool.
    assert!(seen.lock().unwrap().is_empty());
    assert!(b_search.ran.lock().unwrap().is_empty());
    let b_requests = plain_scripted.requests();
    assert_eq!(b_requests.len(), 2);
    let first = &b_requests[1];
    // The rewound request declares the registry in full, with no search.
    assert_eq!(
        first
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        ["a"]
    );
    assert!(
        first.conversation.iter().any(|input| matches!(
            input,
            Input::ToolResult { text, is_error, .. } if text == "Loaded: a." && !is_error
        )),
        "the rewound request renders the search as text: {:?}",
        first.conversation
    );
    assert!(
        first.tools.iter().all(|tool| tool.name != "tool_search"),
        "no tool_search is declared: {:?}",
        first.tools
    );
}

#[test]
fn a_switch_off_and_back_renders_each_way() {
    use contract::commands::ModelArgs;

    const PLAIN: &str = "fake/plain";
    const DEFERRING: &str = "fake/model";
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(vec![SearchTool::output(&["a"])].into()),
        ..SearchScript::default()
    });
    let plain_scripted = Arc::new(ScriptedProvider::new(vec![
        Scripted::text("Plain."),
        Scripted::text("Back."),
    ]));
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            Scripted::text("Home."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    let deferring = Arc::clone(&harness.scripted);
    let plain = Arc::clone(&plain_scripted);
    let prepare: r#loop::Prepare = Arc::new(
        move |args: &ModelArgs, _: Option<&str>, chosen: Option<contract::ThinkingLevel>| {
            let provider: Arc<dyn Provider> = if args.model == DEFERRING {
                Arc::new(Deferring {
                    inner: Arc::clone(&deferring),
                }) as Arc<dyn Provider>
            } else {
                Arc::clone(&plain) as Arc<dyn Provider>
            };
            Ok(r#loop::Prepared {
                provider,
                model: r#loop::Model {
                    reference: args.model.clone(),
                    cost: None,
                    subscription: false,
                },
                thinking: None,
                chosen,
                credential: Some("work".into()),
                cache_lifetime: CacheLifetime::OneHour,
                context_window: fakes::CONTEXT_WINDOW,
                addendum: None,
                handoff: r#loop::HandoffSettings::default(),
                reviewer: Err(contract::shapes::Failure {
                    code: ErrorCode::NoModel,
                    message: r#loop::NO_MODEL_MESSAGE.into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                web_search: r#loop::Hosted::Keep,
                notice: None,
                applied: None,
                credential_files: Vec::new(),
                observe: None,
                reviewer_observe: None,
            })
        },
    );
    harness.looped = harness
        .looped
        .take()
        .map(|looped| looped.switcher(prepare, r#loop::Switchable { chosen: None }));

    // On the deferring provider the search renders natively.
    assert_eq!(harness.turn("hi"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_eq!(completed(&lines).len(), 1);
    assert_eq!(harness.requests().len(), 2);

    // Off: the same search renders as text, and no search is declared.
    harness.inbox.send(model(PLAIN, None)).unwrap();
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    assert_eq!(deferred_names(&lines), Vec::<String>::new());
    assert_eq!(declared_names(&lines), ["a"]);
    // The rebuild dropped the search it no longer declares: one removal
    // published, and no second declare.
    assert_eq!(harness.rows.removed("tool_search"), 1);
    assert_eq!(harness.rows.declared("tool_search"), 1);
    let off = &plain_scripted.requests()[0];
    assert!(
        off.conversation.iter().any(|input| matches!(
            input,
            Input::ToolResult { text, is_error, .. } if text == "Loaded: a." && !is_error
        )),
        "the search renders as text off deferral: {:?}",
        off.conversation
    );

    // Back on: the search renders natively and is declared again.
    harness.inbox.send(model(DEFERRING, None)).unwrap();
    assert_eq!(harness.turn("home"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    assert_eq!(deferred_names(&lines), ["a"]);
    assert_eq!(declared_names(&lines), ["a", "tool_search"]);
    // Back on, the search declares again; the earlier removal stands.
    assert_eq!(harness.rows.declared("tool_search"), 2);
    assert_eq!(harness.rows.removed("tool_search"), 1);
    let on_requests = harness.requests();
    let on = on_requests.last().unwrap();
    assert!(matches!(
        on.conversation.iter().find(|input| matches!(input, Input::ToolSearchResult { .. })),
        Some(Input::ToolSearchResult { loaded, .. }) if loaded == &vec!["a".to_owned()]
    ));
}

#[test]
fn a_cancelled_search_loads_nothing_and_renders_the_cancel_line() {
    let search = Arc::new(SearchScript {
        outputs: Mutex::new(
            vec![Output {
                content: vec![ContentPart::Text {
                    text: "Loaded: a.".into(),
                }],
                loaded: Some(vec!["a".into()]),
                ..Output::default()
            }]
            .into(),
        ),
        wait_cancel: true,
        ..SearchScript::default()
    });
    let mut harness = Harness::start(
        vec![
            calls_reply("", &[("tool_search", search_arguments("a"))]),
            Scripted::text("Done."),
            Scripted::text("After."),
        ],
        vec![("fx".to_owned(), Arc::new(CatalogTool::deferred("a")))],
        Arc::clone(&search),
        false,
        None,
    );
    harness.inbox.send(delivery("hi")).unwrap();
    let tap = support::Tap::new(&harness.log);
    let cancel = Arc::clone(&harness.cancel);
    let mut looped = harness.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    tap.wait_for("tool_call_started");
    assert!(cancel.cancel());
    let (looped, outcome) = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    harness.looped = Some(looped);
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ]],
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "cancelled");
    assert!(done[0].payload.get("loaded").is_none());
    let cancelled_line = "The search was cancelled; no tool was loaded.";
    assert_eq!(text_all(done[0]), cancelled_line);
    // The interrupted turn sent no second request.
    assert_eq!(harness.requests().len(), 1);
    // The next request carries the cancel line as a plain result.
    assert_eq!(harness.turn("again"), Some(TurnOutcome::Completed));
    let lines = harness.lines();
    assert_kinds(
        &lines,
        &[&[
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]],
    );
    let requests = harness.requests();
    let next = &requests[1].conversation;
    assert!(
        !next
            .iter()
            .any(|input| matches!(input, Input::ToolSearchResult { .. })),
        "no search result follows a cancelled search"
    );
    let result = next
        .iter()
        .find(|input| matches!(input, Input::ToolResult { .. }))
        .expect("one cancel result");
    let Input::ToolResult { text, is_error, .. } = result else {
        panic!("expected a plain ToolResult, got {result:?}")
    };
    assert_eq!(text, cancelled_line);
    assert!(!is_error);
    assert!(
        !harness
            .rows
            .states("a")
            .contains(&contract::events::ToolState::Loaded)
    );
}
