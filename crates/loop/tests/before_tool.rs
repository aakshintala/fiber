//! `before_tool` in the loop (`docs/extensions.md`, "Hooks"): the reviewer
//! judges the call that will run, and a resume prepares a suspended call
//! again or runs a started one again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{
    AskStep, Decision, Escalation, Event, FiberExited, Interaction, PermissionRequested, RuleOffer,
    ToolCallRequested, TurnOutcome,
};
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall,
    BeforeToolOutcome, CheckAnswer, CheckOutcome, Hooks,
};
use contract::inbox::{Ack, Delivery};
use contract::provider::{Input, Provider};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Question, Usage};
use contract::tool::{Answered, Ask, Asking, Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, RequestId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use serde_json::{Map, Value, json};

use support::{
    DEADLINE, History, Session, Tap, calls_reply, delivery, ignore, message_started, of_kind,
    standing_request, user_turn,
};

/// What a hook was shown, owned.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    tool: String,
    arguments: Map<String, Value>,
}

/// A scripted `Hooks` fake: seen calls and a queue of answers per method,
/// with an optional shutdown to start inside `before_tool`.
struct FakeHooks {
    before_seen: Mutex<Vec<Seen>>,
    check_seen: Mutex<Vec<Seen>>,
    before_answers: Mutex<VecDeque<BeforeToolOutcome>>,
    shutdown: Mutex<Option<Arc<r#loop::TurnCancel>>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            before_seen: Mutex::new(Vec::new()),
            check_seen: Mutex::new(Vec::new()),
            before_answers: Mutex::new(VecDeque::new()),
            shutdown: Mutex::new(None),
        }
    }

    fn rewriting(arguments: Value) -> Self {
        let hooks = Self::new();
        hooks
            .before_answers
            .lock()
            .unwrap()
            .push_back(BeforeToolOutcome::Rewritten {
                arguments: arguments.as_object().unwrap().clone(),
                changed_by: vec!["fiber.test/guard".to_owned()],
            });
        hooks
    }

    fn before_len(&self) -> usize {
        self.before_seen.lock().unwrap().len()
    }

    fn check_len(&self) -> usize {
        self.check_seen.lock().unwrap().len()
    }
}

fn seen_of(call: &BeforeToolCall<'_>) -> Seen {
    Seen {
        tool: call.tool.to_owned(),
        arguments: call.arguments.clone(),
    }
}

impl Hooks for FakeHooks {
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

    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.before_seen.lock().unwrap().push(seen_of(call));
        if let Some(cancel) = self.shutdown.lock().unwrap().clone() {
            cancel.shutdown(143);
        }
        let outcome = self
            .before_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(BeforeToolOutcome::Unchanged);
        BeforeToolAnswer {
            outcome,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer {
        self.check_seen.lock().unwrap().push(seen_of(call));
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        BeforeMessageAnswer {
            outcome: BeforeMessageOutcome::Passed {
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

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// A tool whose declared paths come from its `path` argument, recording
/// every run. `always_reviewed` sends its calls to step 7.
struct PathTool {
    always_reviewed: bool,
    runs: Mutex<Vec<Map<String, Value>>>,
    output: String,
}

impl PathTool {
    fn reviewed() -> Self {
        Self {
            always_reviewed: true,
            runs: Mutex::new(Vec::new()),
            output: "read it".to_owned(),
        }
    }

    fn plain() -> Self {
        Self {
            always_reviewed: false,
            ..Self::reviewed()
        }
    }

    fn runs(&self) -> Vec<Map<String, Value>> {
        self.runs.lock().unwrap().clone()
    }
}

impl Tool for PathTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".to_owned(),
            description: "Reads the path.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // A test-only branch: the hook rewrote the call into arguments the
        // schema fits but the tool cannot classify.
        if path == "bad/effects" {
            return Err(EffectsError::Tool("the path resolves nowhere".into()));
        }
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: Some(vec![path.clone()]),
                confined: false,
            },
            subject: Some(path),
            prefix: None,
            always_reviewed: self.always_reviewed,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(&self, arguments: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.runs.lock().unwrap().push(arguments.clone());
        Output {
            content: vec![ContentPart::Text {
                text: self.output.clone(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn hook(hooks: Arc<FakeHooks>, session: &mut Session) {
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.hooks(hooks as Arc<dyn Hooks>));
}

fn reviewer_texts(texts: &[&str]) -> Vec<Scripted> {
    texts.iter().map(|text| Scripted::text(text)).collect()
}

fn user_inputs(conversation: &[Input]) -> Vec<&Input> {
    conversation
        .iter()
        .filter(|input| matches!(input, Input::User { .. }))
        .collect()
}

fn user_text(input: &Input) -> &str {
    match input {
        Input::User { text, .. } => text,
        Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. } => {
            panic!("a user item: {input:?}")
        }
    }
}

/// A session whose first reply makes `calls`, reviewed with `review`,
/// under hooks rewriting every `probe` call to `b.txt`.
fn reviewed_session(
    review: Vec<Scripted>,
    calls: Vec<(&str, Value)>,
) -> (
    Session,
    Arc<PathTool>,
    Arc<FakeHooks>,
    Arc<ScriptedProvider>,
) {
    let tool = Arc::new(PathTool::reviewed());
    let script: Vec<Scripted> = vec![calls_reply("", &calls), Scripted::text("Done.")];
    let mut session = Session::with_tools(script, None, vec![tool.clone() as Arc<dyn Tool>]);
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "b.txt"})));
    hook(Arc::clone(&hooks), &mut session);
    let reviewer = session.reviewer(review);
    (session, tool, hooks, reviewer)
}

#[test]
fn a_rewritten_call_is_reviewed_as_the_call_that_will_run() {
    let (mut session, tool, _hooks, reviewer) = reviewed_session(
        reviewer_texts(&["check", "allow looks fine"]),
        vec![("probe", json!({"path": "a.txt"}))],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(tool.runs().len(), 1);

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    let first = &requests[0].conversation;
    let second = &requests[1].conversation;
    assert_eq!(first.len(), second.len());
    // Both stages send the same bytes up to the stage instruction.
    assert_eq!(&first[..first.len() - 1], &second[..second.len() - 1]);

    let users = user_inputs(first);
    // The call's item is the model sent it.
    let call = user_text(users[users.len() - 3]);
    assert_eq!(
        call,
        "Tool call: {\"tool\":\"probe\",\"arguments\":{\"path\":\"a.txt\"}}"
    );
    // Then the rewritten call's effects and the arguments it runs with.
    let effects = user_text(users[users.len() - 2]);
    let mut lines = effects.lines();
    // The first line names the call under review.
    assert_eq!(
        lines.next().unwrap(),
        "Call under review: the last tool call above, probe."
    );
    let declared: Value = serde_json::from_str(
        lines
            .next()
            .unwrap()
            .strip_prefix("Declared effects: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(declared["paths"], json!(["b.txt"]));
    assert_eq!(
        lines.next().unwrap(),
        "Extension fiber.test/guard rewrote this call's arguments; it runs with {\"path\":\"b.txt\"}"
    );
    assert!(lines.next().unwrap().starts_with("Workspace root: "));
    assert!(lines.next().is_none());
    assert_eq!(
        turn_kinds(&session.lines()),
        first_turn_kinds(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed"
            ],
            2
        )
    );
}

/// A blocked rewritten call logs the judged arguments with who rewrote
/// them: the decision carries the rewrite, and nothing started.
#[test]
fn a_blocked_rewritten_call_logs_the_arguments_it_was_judged_with() {
    let (mut session, tool, _hooks, _reviewer) = reviewed_session(
        reviewer_texts(&["check", "block no"]),
        vec![("probe", json!({"path": "a.txt"}))],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        turn_kinds(&lines),
        first_turn_kinds(&["permission_resolved", "tool_call_completed"], 2)
    );
    let resolved = line(&lines, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "reviewer");
    assert_eq!(resolved.payload.get("request_id"), None);
    assert_eq!(resolved.payload["effects"], json!(["reads"]));
    assert_eq!(resolved.payload["reversible"], true);
    assert_eq!(resolved.payload["paths"], json!(["b.txt"]));
    assert_eq!(resolved.payload["arguments"], json!({"path": "b.txt"}));
    assert_eq!(resolved.payload["changed_by"], json!(["fiber.test/guard"]));
    let flat = serde_json::to_string(&resolved.payload).unwrap();
    assert!(!flat.contains("a.txt"), "{flat}");
    assert!(tool.runs().is_empty());
}

#[test]
fn an_unrewritten_reviewed_call_has_no_rewrite_line() {
    let tool = Arc::new(PathTool::reviewed());
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("probe", json!({"path": "a.txt"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    hook(Arc::new(FakeHooks::new()), &mut session);
    let reviewer = session.reviewer(reviewer_texts(&["allow"]));
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 1);
    let users = user_inputs(&requests[0].conversation);
    let effects = user_text(users[users.len() - 2]);
    assert!(!effects.contains("rewrote"), "{effects}");
    assert_eq!(effects.lines().count(), 3);
    assert_eq!(
        turn_kinds(&session.lines()),
        first_turn_kinds(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed"
            ],
            1
        )
    );
}

#[test]
fn a_second_review_renders_the_first_call_as_its_own_review_did() {
    let tool = Arc::new(PathTool::reviewed());
    let mut session = Session::with_tools(
        vec![
            calls_reply(
                "",
                &[
                    ("probe", json!({"path": "a.txt"})),
                    ("probe", json!({"path": "c.txt"})),
                ],
            ),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let hooks = FakeHooks::new();
    for path in ["b.txt", "d.txt"] {
        hooks
            .before_answers
            .lock()
            .unwrap()
            .push_back(BeforeToolOutcome::Rewritten {
                arguments: json!({"path": path}).as_object().unwrap().clone(),
                changed_by: vec!["fiber.test/guard".to_owned()],
            });
    }
    hook(Arc::new(hooks), &mut session);
    let reviewer = session.reviewer(reviewer_texts(&["allow", "allow"]));
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));

    let requests = reviewer.requests();
    assert_eq!(requests.len(), 2);
    let first = user_inputs(&requests[0].conversation);
    let second = user_inputs(&requests[1].conversation);
    // The second review's item for the first call is byte-identical to
    // that item in the first call's own review, with no rewrite line.
    assert_eq!(second[second.len() - 4], first[first.len() - 3]);
    assert!(!user_text(second[second.len() - 4]).contains("rewrote"));
    assert_eq!(
        turn_kinds(&session.lines()),
        vec![
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
            "usage_recorded",
            "permission_resolved",
            "usage_recorded",
            "permission_resolved",
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
        ]
    );
}

#[test]
fn a_blocked_call_renders_identically_after_a_resume() {
    let tool = Arc::new(PathTool::reviewed());
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("probe", json!({"path": "a.txt"}))]),
            Scripted::text("Done."),
            calls_reply("", &[("probe", json!({"path": "c.txt"}))]),
            Scripted::text("Done."),
            calls_reply("", &[("probe", json!({"path": "e.txt"}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn Tool>],
    );
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "b.txt"})));
    hook(Arc::clone(&hooks), &mut session);
    let blocked = session.reviewer(reviewer_texts(&["check", "block: too risky"]));
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert!(tool.runs().is_empty());
    assert_eq!(
        turn_kinds(&session.lines()),
        first_turn_kinds(&["permission_resolved", "tool_call_completed"], 2)
    );

    let live = session.reviewer(reviewer_texts(&["allow"]));
    session.inbox.send(delivery("more")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        turn_kinds(&session.lines()),
        next_turn_kinds(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed"
            ],
            1,
            false
        )
    );
    let live_request = live.requests()[0].conversation.clone();
    let _ = blocked;

    session.resume(vec![tool.clone() as Arc<dyn Tool>]);
    hook(Arc::clone(&hooks), &mut session);
    let resumed = session.reviewer(reviewer_texts(&["allow"]));
    session.inbox.send(delivery("third")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        turn_kinds(&session.lines()),
        next_turn_kinds(
            &[
                "permission_resolved",
                "tool_call_started",
                "tool_call_completed"
            ],
            1,
            true
        )
    );
    let resumed_request = resumed.requests()[0].conversation.clone();

    let live_call = live_request.iter().find(|input| {
        user_text_or_none(input).is_some_and(|text| text.contains("\"path\":\"a.txt\""))
    });
    let resumed_call = resumed_request.iter().find(|input| {
        user_text_or_none(input).is_some_and(|text| text.contains("\"path\":\"a.txt\""))
    });
    assert_eq!(live_call, resumed_call);
}

fn user_text_or_none(input: &Input) -> Option<&str> {
    match input {
        Input::User { text, .. } => Some(text),
        Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. } => None,
    }
}

/// The complete, ordered kinds of `lines`: every line the turn wrote, so
/// a duplicated, missing or reordered line fails.
fn turn_kinds(lines: &[contract::Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

/// The complete kinds of a later turn whose reply says nothing and calls
/// once: as [`first_turn_kinds`], without the session's opening lines.
/// A resumed session's first turn rebuilds its preamble first.
fn next_turn_kinds<'a>(middle: &[&'a str], reviews: usize, resumed: bool) -> Vec<&'a str> {
    let mut kinds = Vec::new();
    if resumed {
        kinds.extend(["fiber_started", "preamble_built"].iter().copied());
    }
    kinds.extend(
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
        ]
        .iter()
        .copied(),
    );
    kinds.extend(std::iter::repeat_n("usage_recorded", reviews));
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
/// The complete kinds of a first turn whose reply says nothing and calls
/// once: the session opens, one reply segment runs, `reviews` reviewer
/// usages follow the reply, `middle` decides the call, and the turn says
/// "Done.".
fn first_turn_kinds<'a>(middle: &[&'a str], reviews: usize) -> Vec<&'a str> {
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
    kinds.extend(std::iter::repeat_n("usage_recorded", reviews));
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

fn probe_requested(path: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "probe".into(),
        arguments: json!({"path": path}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn fiber_exited_suspended(suspended_on: &str) -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: Some(RequestId(suspended_on.into())),
        questions: None,
    })
}

fn review_request(
    request_id: &str,
    paths: &[&str],
    escalation_reason: &str,
    rule_subject: &str,
) -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
        request_id: RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Reads],
            reversible: true,
            paths: Some(paths.iter().map(|path| (*path).to_owned()).collect()),
            confined: false,
        },
        step: AskStep::Review {
            escalation: Some(Escalation::ConsecutiveBlocks {
                reason: escalation_reason.to_owned(),
            }),
            rule: Some(RuleOffer {
                subject: rule_subject.to_owned(),
                prefix: rule_subject.to_owned(),
            }),
        },
    })
}

impl History {
    fn tid(&self) -> contract::TurnId {
        contract::TurnId("t_1".into())
    }

    fn write(&self, event: Event, action: Option<&str>) {
        self.log
            .append(
                &event,
                Some(self.tid()),
                action.map(|action| ActionId(action.into())),
            )
            .unwrap();
    }

    fn freeze(&mut self) {
        self.history_len = self.lines().len();
    }

    fn model() -> r#loop::Model {
        r#loop::Model {
            reference: support::MODEL.into(),
            cost: None,
            subscription: false,
        }
    }

    fn prompt(&self) -> r#loop::PromptInputs {
        let clock = Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>;
        let mut prompt = r#loop::PromptInputs::new(
            self.root.path().to_path_buf(),
            "/bin/sh".into(),
            self.dir.join("events.jsonl").display().to_string(),
            clock,
            fakes::CONTEXT_WINDOW,
        );
        prompt.credential = Some("work".into());
        prompt
    }

    fn resume_with(
        &mut self,
        tools: Vec<(String, Arc<dyn Tool>)>,
        hooks: Arc<FakeHooks>,
        answerable: bool,
    ) -> r#loop::Loop {
        r#loop::Loop::resume(
            r#loop::Session {
                log: Arc::clone(&self.log),
                provider: Arc::clone(&self.provider) as Arc<dyn Provider>,
                model: Self::model(),
                prompt: self.prompt(),
                inbox: self.inbox_rx.take().unwrap(),
                tools,
                permissions: r#loop::Permissions {
                    workspace: self.workspace.clone(),
                    credentials: self.credentials.clone(),
                    credential_files: Vec::new(),
                    rules: self.rules.clone(),
                },
            },
            r#loop::resumed(&self.dir).unwrap(),
        )
        .unwrap()
        .hooks(hooks as Arc<dyn Hooks>)
        .answerable(answerable)
    }

    /// Runs `looped` to its end with `close` queued, through `Loop::run`
    /// so its real status observer folds the log: the `session_status`
    /// lines in order, one per change. The finishing turn defers the
    /// close, and the next idle wait takes it and ends the run.
    #[track_caller]
    fn run_closed(&mut self, looped: r#loop::Loop) -> (r#loop::Ran, Vec<contract::Envelope>) {
        self.inbox_tx.send(Delivery::Close(ignore())).unwrap();
        let mut watcher = self.log.watch();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let ran = looped.run();
            done.send(ran).unwrap();
        });
        let ran = Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the run ended in time");
        ran.result.as_ref().unwrap();
        let mut statuses = Vec::new();
        while let Ok(Some(line)) = watcher.try_recv() {
            if line.kind == "session_status" {
                statuses.push(line);
            }
        }
        (ran, statuses)
    }

    #[track_caller]
    fn step(&mut self, mut looped: r#loop::Loop) -> (r#loop::Loop, Option<TurnOutcome>) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the turn ended in time")
    }

    fn new_lines(&self) -> Vec<contract::Envelope> {
        self.lines()[self.history_len..].to_vec()
    }
}

/// A suspended history: one turn whose batch holds `probe` calls with
/// `paths`, a standing ask `r_9` pending on the first, then the process
/// exited on that request.
fn suspended_probes(paths: &[&str]) -> History {
    let mut history = History::new(vec![Scripted::text("Done.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    for (at, path) in paths.iter().enumerate() {
        history.write(probe_requested(path), Some(&format!("a_{}", at + 1)));
    }
    history.write(standing_request("r_9"), Some("a_1"));
    history.write(support::fiber_started(), None);
    history.write(fiber_exited_suspended("r_9"), None);
    history.freeze();
    history
}

fn probe_tools(tool: &Arc<PathTool>) -> Vec<(String, Arc<dyn Tool>)> {
    vec![("builtin".to_owned(), Arc::clone(tool) as Arc<dyn Tool>)]
}

/// Runs `send` once the log holds the re-raised `permission_requested`.
fn on_reraise(history: &History, send: impl FnOnce() + Send + 'static) -> thread::JoinHandle<()> {
    let watcher = history.log.watch();
    thread::spawn(move || {
        support::read_until(watcher, "a re-raised permission_requested line", |line| {
            line.kind == "permission_requested"
        });
        send();
    })
}

fn reply_allow(request_id: &str) -> Delivery {
    let request_id = RequestId(request_id.into());
    Delivery::Reply(
        Reply {
            request_id,
            answer: ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: None,
                remember: None,
            },
        },
        support::ignore(),
    )
}

fn line<'a>(lines: &'a [contract::Envelope], kind: &str) -> &'a contract::Envelope {
    lines.iter().find(|line| line.kind == kind).unwrap()
}

/// The complete, ordered kinds of `new`: every line the resumed turn
/// wrote, so a duplicated, missing or reordered line fails.
fn new_kinds(new: &[contract::Envelope]) -> Vec<&str> {
    new.iter().map(|line| line.kind.as_str()).collect()
}

/// The complete kinds of a resumed turn that re-raises its request, runs
/// the allowed call and says "Done.".
fn allowed_kinds() -> Vec<&'static str> {
    vec![
        "preamble_built",
        "opening_message",
        "permission_requested",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// The complete kinds of a resumed turn whose call settles without its
/// request raised again: the decision line answers the recorded request,
/// the refused call completes denied, and the later call of the batch
/// starts and runs.
fn settled_kinds() -> Vec<&'static str> {
    vec![
        "preamble_built",
        "opening_message",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// The status fold listed the old request pending, then ran a step with it
/// no longer pending: the first status waits on `r_9`, a later one works,
/// and the last names no `r_9`.
#[track_caller]
fn assert_status_resolved(statuses: &[contract::Envelope]) {
    assert!(!statuses.is_empty(), "the run folded some status");
    assert_eq!(statuses[0].payload["state"], "waiting");
    assert_eq!(statuses[0].payload["waiting"]["request_id"], "r_9");
    assert!(
        statuses
            .iter()
            .any(|line| matches!(line.payload["state"].as_str(), Some("tool" | "streaming"))),
        "a step ran past the settled batch"
    );
    let last = statuses.last().unwrap();
    let names_r9 = last
        .payload
        .get("waiting")
        .and_then(|waiting| waiting.get("request_id"))
        == Some(&json!("r_9"));
    assert!(!names_r9, "the last status names no r_9");
}

/// Writes the resumed process's exit as a shutdown would, and asserts the
/// `fiber_exited` line suspends on nothing: the settled request is answered.
#[track_caller]
fn assert_exited_settled(history: &History, summary: &r#loop::Summary) {
    r#loop::fiber_exited(&history.log, summary, Ok(()), false, Some(143)).unwrap();
    let exited = history
        .lines()
        .into_iter()
        .rfind(|line| line.kind == "fiber_exited")
        .expect("the exit was written");
    assert!(
        exited.payload.get("suspended_on").is_none(),
        "suspended_on names no settled request: {:?}",
        exited.payload
    );
}

/// The complete kinds of a resumed turn whose call never runs: the
/// decision line answers the recorded request by hook, and the call
/// completes without running.
fn refused_kinds() -> Vec<&'static str> {
    vec![
        "preamble_built",
        "opening_message",
        "permission_resolved",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// The complete kinds of a resumed turn that raises its request again and
/// completes its call without running it: as [`allowed_kinds`], without
/// `tool_call_started`.
fn answered_kinds() -> Vec<&'static str> {
    allowed_kinds()
        .into_iter()
        .filter(|kind| *kind != "tool_call_started")
        .collect()
}

fn rewrite_line(names: &str, arguments: &str) -> String {
    format!("Extension {names} rewrote this call's arguments; it ran with {arguments}.")
}

#[test]
fn a_resumed_rewrite_is_raised_judged_and_run_again() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "b.txt"})));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(reply_allow("r_9")).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    let new = history.new_lines();
    let raised: Vec<_> = new
        .iter()
        .filter(|line| line.kind == "permission_requested")
        .collect();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].payload["request_id"], "r_9");
    assert_eq!(raised[0].payload["paths"], json!(["b.txt"]));
    assert_eq!(new_kinds(&new), allowed_kinds());
    assert_eq!(raised[0].payload["standing_rule"]["prefix"], "run tests");
    assert_eq!(
        tool.runs(),
        vec![json!({"path": "b.txt"}).as_object().unwrap().clone()]
    );
    let started = line(&new, "tool_call_started");
    assert_eq!(started.payload["arguments"], json!({"path": "b.txt"}));
    assert_eq!(started.payload["changed_by"], json!(["fiber.test/guard"]));
    let completed = line(&new, "tool_call_completed");
    let text = completed.payload["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        format!(
            "{}\nread it",
            rewrite_line("fiber.test/guard", "{\"path\":\"b.txt\"}")
        )
    );
}

#[test]
fn a_resumed_refusal_answers_its_request_by_hook_and_runs_the_rest() {
    let mut history = suspended_probes(&["a.txt", "c.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = FakeHooks::new();
    hooks
        .before_answers
        .lock()
        .unwrap()
        .push_back(BeforeToolOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/guard".to_owned(),
            reason: "no writes".to_owned(),
        }));
    let hooks = Arc::new(hooks);
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let (run, statuses) = history.run_closed(looped);
    assert_status_resolved(&statuses);

    let new = history.new_lines();
    assert!(
        !new.iter().any(|line| line.kind == "permission_requested"),
        "the request is not raised again"
    );
    assert_eq!(new_kinds(&new), settled_kinds());
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.action_id, Some(ActionId("a_1".into())));
    assert_eq!(
        serde_json::Value::Object(resolved.payload.clone()),
        json!({
            "request_id": "r_9",
            "decision": "deny",
            "decided_by": "hook",
            "reason": "Extension fiber.test/guard refused this call: no writes. It did not run.",
        })
    );
    let completed: Vec<_> = new
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].payload["status"], "denied");
    assert_eq!(completed[0].payload["reason"], "hook");
    assert_eq!(
        completed[0].payload["content"][0]["text"],
        "Extension fiber.test/guard refused this call: no writes. It did not run."
    );
    assert_eq!(completed[1].payload["status"], "completed");
    assert_eq!(tool.runs().len(), 1);
    assert_exited_settled(&history, &run.summary);
}

#[test]
fn a_resumed_hook_failure_answers_its_request_by_hook_and_fails_the_call() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = FakeHooks::new();
    hooks
        .before_answers
        .lock()
        .unwrap()
        .push_back(BeforeToolOutcome::Failed(contract::hook::HookFailure {
            extension: "fiber.test/guard".to_owned(),
            message: "boom".to_owned(),
        }));
    let hooks = Arc::new(hooks);
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let (run, statuses) = history.run_closed(looped);
    assert_status_resolved(&statuses);

    let new = history.new_lines();
    assert!(
        !new.iter().any(|line| line.kind == "permission_requested"),
        "the request is not raised again"
    );
    assert_eq!(new_kinds(&new), refused_kinds());
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.action_id, Some(ActionId("a_1".into())));
    assert_eq!(
        serde_json::Value::Object(resolved.payload.clone()),
        json!({
            "request_id": "r_9",
            "decision": "deny",
            "decided_by": "hook",
            "reason": "The `before_tool` hook of extension fiber.test/guard failed, \
                       so the call did not run: boom",
        })
    );
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "hook_failed");
    assert!(tool.runs().is_empty(), "the call never runs");
    assert_exited_settled(&history, &run.summary);
}

#[test]
fn a_resumed_rewrite_the_schema_rejects_answers_its_request_by_hook() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": 5})));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let (run, statuses) = history.run_closed(looped);
    assert_status_resolved(&statuses);

    let new = history.new_lines();
    assert!(
        !new.iter().any(|line| line.kind == "permission_requested"),
        "the request is not raised again"
    );
    assert_eq!(new_kinds(&new), refused_kinds());
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "hook");
    let reason = resolved.payload["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(
            "Extension fiber.test/guard rewrote this call's arguments, \
             and they do not match the tool's schema:"
        ),
        "{reason}"
    );
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "invalid_arguments");
    assert!(tool.runs().is_empty(), "the call never runs");
    assert_exited_settled(&history, &run.summary);
}

#[test]
fn a_resumed_rewrite_its_effects_reject_answers_its_request_by_hook() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "bad/effects"})));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let (run, statuses) = history.run_closed(looped);
    assert_status_resolved(&statuses);

    let new = history.new_lines();
    assert!(
        !new.iter().any(|line| line.kind == "permission_requested"),
        "the request is not raised again"
    );
    assert_eq!(new_kinds(&new), refused_kinds());
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "hook");
    let reason = resolved.payload["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(
            "Extension fiber.test/guard rewrote this call's arguments, \
             and the tool could not classify them:"
        ),
        "{reason}"
    );
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "tool_error");
    assert!(tool.runs().is_empty(), "the call never runs");
    assert_exited_settled(&history, &run.summary);
}

#[test]
fn a_shutdown_inside_the_resumed_hook_writes_no_decision() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let cancel = Arc::new(r#loop::TurnCancel::default());
    let hooks = FakeHooks::new();
    hooks.shutdown.lock().unwrap().replace(Arc::clone(&cancel));
    hooks
        .before_answers
        .lock()
        .unwrap()
        .push_back(BeforeToolOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/guard".to_owned(),
            reason: "no writes".to_owned(),
        }));
    let hooks = Arc::new(hooks);
    let looped = history
        .resume_with(probe_tools(&tool), Arc::clone(&hooks), true)
        .cancelled_by(cancel);
    let (_looped, outcome) = history.step(looped);
    assert_eq!(outcome, None);

    let new = history.new_lines();
    assert_eq!(new_kinds(&new), ["preamble_built", "opening_message"]);
}

#[test]
fn a_resumed_rewrite_into_credentials_is_refused_without_asking() {
    let mut history = suspended_probes(&["a.txt"]);
    let secret = history.credentials.join("secret.txt");
    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(
        json!({"path": secret.display().to_string()}),
    ));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let (_looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    let new = history.new_lines();
    assert!(
        !new.iter().any(|line| line.kind == "permission_requested"),
        "the request is not raised again"
    );
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.payload["request_id"], "r_9");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "credential_deny");
    assert_eq!(new_kinds(&new), refused_kinds());
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "denied");
    assert_eq!(completed.payload["reason"], "credentials");
    assert!(tool.runs().is_empty());
}

#[test]
fn a_call_gone_at_resume_raises_its_request_and_fails_on_allow() {
    let mut history = suspended_probes(&["a.txt"]);
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "b.txt"})));
    let looped = history.resume_with(Vec::new(), Arc::clone(&hooks), true);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(reply_allow("r_9")).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    // No hook runs on a call that never checks.
    assert_eq!(hooks.before_len(), 0);

    let new = history.new_lines();
    let raised: Vec<_> = new
        .iter()
        .filter(|line| line.kind == "permission_requested")
        .collect();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].payload["request_id"], "r_9");
    assert!(raised[0].payload.get("paths").is_none());
    assert_eq!(new_kinds(&new), answered_kinds());
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "failed");
    assert_eq!(completed.payload["error"]["code"], "unknown_tool");
}

#[test]
fn a_headless_resume_denies_the_rebuilt_request_by_cancel() {
    let mut history = suspended_probes(&["a.txt"]);
    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "b.txt"})));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), false);
    let (_looped, outcome) = history.step(looped);
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    let new = history.new_lines();
    let raised = line(&new, "permission_requested");
    assert_eq!(raised.payload["request_id"], "r_9");
    assert_eq!(raised.payload["paths"], json!(["b.txt"]));
    let resolved = line(&new, "permission_resolved");
    assert_eq!(resolved.payload["request_id"], "r_9");
    assert_eq!(resolved.payload["decision"], "deny");
    assert_eq!(resolved.payload["decided_by"], "cancel");
    assert_eq!(
        resolved.payload["reason"],
        "The session was resumed with nobody to answer."
    );
    assert_eq!(new_kinds(&new), answered_kinds());
    let completed = line(&new, "tool_call_completed");
    assert_eq!(completed.payload["status"], "denied");
    assert!(tool.runs().is_empty());
}

#[test]
fn a_resumed_review_rebuilds_its_rule_from_the_prepared_call() {
    let mut history = History::new(vec![Scripted::text("Done.")]);
    history.write(user_turn("one"), None);
    history.write(message_started(), Some("a_0"));
    history.write(probe_requested("b.txt"), Some("a_1"));
    history.write(
        review_request("r_9", &["b.txt"], "too risky", "b.txt"),
        Some("a_1"),
    );
    history.write(support::fiber_started(), None);
    history.write(fiber_exited_suspended("r_9"), None);
    history.freeze();

    let tool = Arc::new(PathTool::plain());
    let hooks = Arc::new(FakeHooks::rewriting(json!({"path": "c.txt"})));
    let looped = history.resume_with(probe_tools(&tool), Arc::clone(&hooks), true);
    let inbox = history.inbox_tx.clone();
    let replied = on_reraise(&history, move || inbox.send(reply_allow("r_9")).unwrap());
    let (_looped, outcome) = history.step(looped);
    replied.join().unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Completed));

    let new = history.new_lines();
    let raised = line(&new, "permission_requested");
    assert_eq!(raised.payload["request_id"], "r_9");
    assert_eq!(raised.payload["step"], "review");
    assert_eq!(raised.payload["escalation"]["cause"], "consecutive_blocks");
    assert_eq!(raised.payload["escalation"]["reason"], "too risky");
    assert_eq!(raised.payload["paths"], json!(["c.txt"]));
    assert_eq!(raised.payload["rule"]["subject"], "c.txt");
    assert_eq!(new_kinds(&new), allowed_kinds());
    assert_eq!(
        tool.runs(),
        vec![json!({"path": "c.txt"}).as_object().unwrap().clone()]
    );
}

/// A tool that asks one suspending form, recording every run. Its schema
/// is `schema`: `Strict` runs the L14b resume with a schema the started
/// arguments fail.
struct AskTool {
    schema: Value,
    runs: Mutex<Vec<Map<String, Value>>>,
}

impl AskTool {
    fn loose() -> Self {
        Self {
            schema: json!({"type": "object"}),
            runs: Mutex::new(Vec::new()),
        }
    }

    fn strict() -> Self {
        Self {
            schema: json!({
                "type": "object",
                "properties": {"extra": {"type": "integer"}},
                "required": ["extra"],
                "additionalProperties": true
            }),
            runs: Mutex::new(Vec::new()),
        }
    }

    fn runs(&self) -> Vec<Map<String, Value>> {
        self.runs.lock().unwrap().clone()
    }
}

fn to_driver(questions: Vec<Question>) -> Output {
    Output {
        control: Some(contract::events::Control {
            handoff: None,
            questions: Some(questions),
            skill: None,
            name: None,
        }),
        ..Output {
            content: vec![ContentPart::Text {
                text: "The questions went to the driver.".into(),
            }],
            ..Output::default()
        }
    }
}

impl Tool for AskTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "asker".into(),
            description: "Asks the person a form.".into(),
            input_schema: self.schema.clone(),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            review_when_confined: false,
            hosts: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        panic!("the loop runs a call through run_asking")
    }

    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        self.runs.lock().unwrap().push(arguments.clone());
        let questions: Vec<Question> =
            serde_json::from_value(arguments["questions"].clone()).unwrap();
        if !ask.answerable() {
            return to_driver(questions);
        }
        let answered = ask.ask(Asking {
            interaction: Interaction::Form {
                fields: questions.clone(),
            },
            action_ids: Vec::new(),
            until: None,
            check: None,
            suspends: true,
        });
        match answered {
            Answered::Reply(answer) => Output {
                content: vec![ContentPart::Text {
                    text: serde_json::to_string(&answer).unwrap(),
                }],
                ..Output::default()
            },
            Answered::NoAnswer if cancel.is_cancelled() => Output {
                content: vec![ContentPart::Text {
                    text: "declined".into(),
                }],
                ..Output::default()
            },
            Answered::NoAnswer => to_driver(questions),
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A tool whose call returns only once its gate opens.
struct Blocker {
    gate: Arc<support::Gate>,
}

impl Tool for Blocker {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "waits".into(),
            description: "Waits.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        AskTool::loose().effects(&Map::new())
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.gate.wait();
        Output {
            content: vec![ContentPart::Text {
                text: "waited".into(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn questions() -> Value {
    json!({"questions": [{
        "header": "Base",
        "question": "Which branch?",
        "options": [{"label": "main"}, {"label": "dev"}],
    }]})
}

fn with_extra(arguments: Value) -> Value {
    let mut map = arguments.as_object().unwrap().clone();
    map.insert("extra".into(), Value::String("rewritten".into()));
    Value::Object(map)
}

type Finished = mpsc::Receiver<(r#loop::Loop, Result<Option<TurnOutcome>, r#loop::Error>)>;

fn run_turn(session: &mut Session) -> Finished {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    let turn = thread::spawn(move || {
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    session.turn_thread = Some(turn.thread().id());
    finished
}

#[track_caller]
fn finish_turn(session: &mut Session, finished: &Finished) -> Option<TurnOutcome> {
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(finished)
        .expect("the turn ended in time");
    session.looped = Some(looped);
    outcome.unwrap()
}

#[track_caller]
fn send_form(session: &Session, request: &str) {
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send(answer);
    }));
    let reply = Reply {
        request_id: RequestId(request.into()),
        answer: serde_json::from_value::<ReplyAnswer>(json!({
            "answers": [{"labels": ["main"]}]
        }))
        .unwrap(),
    };
    session.inbox.send(Delivery::Reply(reply, ack)).unwrap();
    let answered = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the reply is answered");
    assert!(answered.is_ok(), "the form reply was accepted");
}

fn request_id(line: &contract::Envelope) -> String {
    line.payload["request_id"].as_str().unwrap().to_owned()
}

/// A live session whose rewritten `asker` call suspends on its form: the
/// session shuts down on it and exits naming its request. Returns the
/// session, the live asker, the hooks and the request.
#[track_caller]
fn suspended_form() -> (Session, Arc<AskTool>, Arc<FakeHooks>, String) {
    let gate = Arc::new(support::Gate::default());
    let asker = Arc::new(AskTool::loose());
    let blocker = Arc::new(Blocker {
        gate: Arc::clone(&gate),
    });
    let hooks = FakeHooks::new();
    // In reply order: `waits` passes through, `asker` is rewritten.
    hooks
        .before_answers
        .lock()
        .unwrap()
        .push_back(BeforeToolOutcome::Unchanged);
    hooks
        .before_answers
        .lock()
        .unwrap()
        .push_back(BeforeToolOutcome::Rewritten {
            arguments: with_extra(questions()).as_object().unwrap().clone(),
            changed_by: vec!["fiber.test/guard".to_owned()],
        });
    let hooks = Arc::new(hooks);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("waits", json!({})), ("asker", questions())]),
            Scripted::text("Done."),
        ],
        None,
        vec![blocker as Arc<dyn Tool>, asker.clone() as Arc<dyn Tool>],
    )
    .inbox_woken();
    hook(Arc::clone(&hooks), &mut session);
    session.inbox.send(delivery("go")).unwrap();
    // The tap subscribes before the turn starts: the loop can raise its
    // question before a later subscription would see it.
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    let requested = tap.wait_for("interaction_requested");
    assert_eq!(requested.payload["resumes"], true);
    let request = request_id(&requested);
    session.cancel.shutdown(143);
    gate.open();
    assert_eq!(finish_turn(&mut session, &finished), None);
    r#loop::fiber_exited(
        &session.log,
        session.looped.as_ref().unwrap().summary(),
        Ok(()),
        false,
        Some(143),
    )
    .unwrap();
    let lines = session.events_until("fiber_exited", |line| line.kind == "fiber_exited");
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request
    );
    let started: Vec<_> = of_kind(&lines, "tool_call_started")
        .into_iter()
        .filter(|line| line.payload.get("arguments").is_some())
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].payload["arguments"], with_extra(questions()));
    assert_eq!(
        started[0].payload["changed_by"],
        json!(["fiber.test/guard"])
    );
    assert_eq!(
        turn_kinds(&lines),
        vec![
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
            "interaction_requested",
            "tool_call_completed",
            "fiber_exited",
        ]
    );
    (session, asker, hooks, request)
}

#[test]
fn a_started_rewrite_runs_again_unhooked_with_its_result_line() {
    let (mut session, _asker, hooks, request) = suspended_form();
    // Both live calls passed the hooks; the resume must not call them again.
    assert_eq!(hooks.before_len(), 2);
    assert_eq!(hooks.check_len(), 2);

    let asker = Arc::new(AskTool::loose());
    session.resume(vec![asker.clone() as Arc<dyn Tool>]);
    hook(Arc::clone(&hooks), &mut session);
    // As above: subscribe before the finishing turn re-raises its question.
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    let raised = tap.wait_for("interaction_requested");
    assert_eq!(request_id(&raised), request);
    send_form(&session, &request);
    assert_eq!(
        finish_turn(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    // No second start: the call runs again under its first one.
    let lines = session.lines();
    assert_eq!(of_kind(&lines, "tool_call_started").len(), 0);
    assert_eq!(
        turn_kinds(&lines),
        vec![
            "fiber_started",
            "preamble_built",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );

    // Neither hook ran again: the started arguments run as they were.
    assert_eq!(hooks.before_len(), 2);
    assert_eq!(hooks.check_len(), 2);
    assert_eq!(
        asker.runs(),
        vec![with_extra(questions()).as_object().unwrap().clone()]
    );
    let completed: Vec<_> = of_kind(&lines, "tool_call_completed")
        .into_iter()
        .filter(|line| {
            line.payload["content"]
                .as_array()
                .is_some_and(|content| !content.is_empty())
        })
        .collect();
    let text = completed[0].payload["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let arguments = serde_json::to_string(&with_extra(questions())).unwrap();
    assert!(
        text.starts_with(&format!(
            "Extension fiber.test/guard rewrote this call's arguments; it ran with {arguments}.\n"
        )),
        "{text}"
    );
}

#[test]
fn a_started_rewrite_that_no_longer_checks_fails_without_running() {
    let (mut session, _asker, hooks, request) = suspended_form();

    let strict = Arc::new(AskTool::strict());
    session.resume(vec![strict.clone() as Arc<dyn Tool>]);
    hook(Arc::clone(&hooks), &mut session);
    let finished = run_turn(&mut session);
    assert_eq!(
        finish_turn(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let _ = request;

    assert!(strict.runs().is_empty());
    assert_eq!(hooks.before_len(), 2);
    let lines = session.lines();
    // The question is raised again, but its call never runs: Fiber
    // declines the re-raised request and the call fails its re-check.
    assert_eq!(of_kind(&lines, "tool_call_started").len(), 0);
    // The question is raised again, but its call never runs: Fiber
    // declines the re-raised request and the call fails its re-check.
    assert_eq!(of_kind(&lines, "tool_call_started").len(), 0);
    assert_eq!(
        turn_kinds(&lines),
        vec![
            "fiber_started",
            "preamble_built",
            "interaction_requested",
            "interaction_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let completed = of_kind(&lines, "tool_call_completed");
    let failed = completed
        .iter()
        .find(|line| line.payload["status"] == "failed")
        .expect("the call fails");
    assert_eq!(failed.payload["error"]["code"], "invalid_arguments");
    let text = failed.payload["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "Extension fiber.test/guard rewrote this call's arguments, and they do not match the tool's schema:"
        ),
        "{text}"
    );
    assert!(!text.contains("it ran with"), "{text}");
}
