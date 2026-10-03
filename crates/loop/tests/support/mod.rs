//! What the loop's tests and its `turn` jig share: scripted replies on the
//! provider seam, and a session wired to them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "test code, helpers included; each test binary uses some helpers"
)]

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use contract::events::{
    ReasoningCompleted, TextDelta, ToolCallArgumentsDelta, ToolCallRequested, TurnOutcome,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::provider::{Delta, ModelCall, ModelRequest, Provider, ReplyAction, ToolDefinition};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Failure, Origin, Sender as From};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, RequestId, SessionId};
use fakes::{Scripted, ScriptedProvider, reply};
use log::Log;
use r#loop::{BlockLimits, Loop, Model, Reviewer};
use serde_json::{Map, Value, json};

/// How long a turn may take before a test fails instead of hanging.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// The model reference the scripted provider answers as.
pub(crate) const MODEL: &str = "fake/model-1";

/// The model reference a scripted reviewer answers as.
pub(crate) const REVIEWER_MODEL: &str = "fake/reviewer-1";

/// Watches the log for the turn's `permission_requested`, then runs `send`
/// with its request id: the signal the loop is waiting for a reply. The wait
/// is bounded by [`DEADLINE`]: a turn that never asks fails naming the
/// missing `permission_requested`, and the thread's end drops its inbox
/// sender, releasing a loop still waiting for a reply.
pub(crate) fn on_request(
    session: &Session,
    send: impl FnOnce(RequestId) + Send + 'static,
) -> thread::JoinHandle<()> {
    let mut watcher = session.log.watch();
    thread::spawn(move || {
        // The watcher blocks without a deadline, so its lines cross an mpsc
        // channel, as in `Session::open`, and the wait below carries the
        // deadline instead.
        let (forward, waiting) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            let line = waiting
                .recv_timeout(DEADLINE)
                .expect("a permission_requested line");
            if line.kind == "permission_requested" {
                send(RequestId(
                    line.payload["request_id"].as_str().unwrap().into(),
                ));
                return;
            }
        }
    })
}

/// A reply with readable reasoning, then `text`.
pub(crate) fn reasoning_reply(thought: &str, text: &str) -> Scripted {
    let mut end = reply(text);
    end.actions.insert(
        0,
        ReplyAction::Reasoning(ReasoningCompleted {
            text: thought.into(),
            provider_item: Some(reasoning_item(thought)),
        }),
    );
    Scripted {
        deltas: vec![
            Delta::Reasoning(TextDelta {
                text: thought.into(),
            }),
            Delta::Text(TextDelta { text: text.into() }),
        ],
        end: Ok(end),
    }
}

/// A reply that calls `names`, each with `{"city": "Paris"}`, after `text`.
pub(crate) fn tool_call_reply(text: &str, names: &[&str]) -> Scripted {
    let calls: Vec<(&str, Value)> = names
        .iter()
        .map(|n| (*n, json!({"city": "Paris"})))
        .collect();
    calls_reply(text, &calls)
}

/// A reply that makes `calls`, each a tool's name and its arguments, after
/// `text`.
pub(crate) fn calls_reply(text: &str, calls: &[(&str, Value)]) -> Scripted {
    let mut end = reply(text);
    let mut deltas = vec![Delta::Text(TextDelta { text: text.into() })];
    for (index, (name, arguments)) in calls.iter().enumerate() {
        deltas.push(Delta::ToolCallArguments(ToolCallArgumentsDelta {
            index: u32::try_from(index).unwrap(),
            name: Some((*name).into()),
            text: arguments.to_string(),
        }));
        end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
            name: (*name).into(),
            arguments: arguments.clone(),
            provider_id: None,
            repair: None,
        }));
    }
    Scripted {
        deltas,
        end: Ok(end),
    }
}

/// Notes shared by the tools in one step, and a signal when one is added.
/// A tool that waits for another blocks here instead of polling.
pub(crate) struct Trace {
    notes: Mutex<Vec<String>>,
    changed: Condvar,
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            notes: Mutex::default(),
            changed: Condvar::new(),
        }
    }
}

impl Trace {
    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Vec<String>>> {
        self.notes.lock()
    }

    fn push(&self, line: String) {
        let mut notes = self.notes.lock().unwrap();
        notes.push(line);
        self.changed.notify_all();
    }

    /// Blocks until `line` has been pushed, and fails the test at [`DEADLINE`].
    fn wait_for(&self, line: &str) {
        let notes = self.notes.lock().unwrap();
        let (notes, _) = self
            .changed
            .wait_timeout_while(notes, DEADLINE, |notes| {
                !notes.iter().any(|note| note == line)
            })
            .unwrap();
        assert!(
            notes.iter().any(|note| note == line),
            "timed out waiting for {line}"
        );
    }
}

/// A test tool registered through the tool seam, standing in for a built-in.
/// Its schema takes a required string `city` and an optional integer `days`.
pub(crate) struct TestTool {
    pub(crate) name: &'static str,
    /// What its effects function returns.
    pub(crate) effects: Result<DeclaredEffects, EffectsError>,
    /// The call's primary argument, as its tool reads it.
    pub(crate) subject: Option<String>,
    /// The widening a rule would offer.
    pub(crate) prefix: Option<String>,
    /// What a call returns.
    pub(crate) output: Output,
    pub(crate) bound: Bound,
    /// Waited on by every call before it returns.
    pub(crate) barrier: Option<Arc<Barrier>>,
    /// A tool whose call a call of this one waits to see finish, in `trace`.
    pub(crate) after: Option<&'static str>,
    /// What happened, in order, shared between tools: `effects <name>`,
    /// `run <name>`, `done <name>`.
    pub(crate) trace: Arc<Trace>,
    /// The arguments each call ran with.
    pub(crate) ran: Mutex<Vec<Map<String, Value>>>,
    /// What [`Cancel::is_cancelled`] returned at each call.
    pub(crate) cancelled: Mutex<Vec<bool>>,
}

impl TestTool {
    /// A tool whose calls only read, and return `text`.
    pub(crate) fn reads(name: &'static str, text: &str) -> Self {
        Self::declaring(name, text, vec![Effect::Reads], None)
    }

    /// A tool whose calls declare `effects` on `paths`, and return `text`.
    pub(crate) fn declaring(
        name: &'static str,
        text: &str,
        effects: Vec<Effect>,
        paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            name,
            effects: Ok(DeclaredEffects {
                effects,
                reversible: true,
                paths,
            }),
            subject: Some(String::new()),
            prefix: None,
            output: Output {
                content: vec![ContentPart::Text { text: text.into() }],
                ..Output::default()
            },
            bound: Bound::DEFAULT,
            barrier: None,
            after: None,
            trace: Arc::default(),
            ran: Mutex::default(),
            cancelled: Mutex::default(),
        }
    }

    /// A tool whose calls fail with `code`.
    pub(crate) fn failing(name: &'static str, code: contract::ErrorCode) -> Self {
        let mut tool = Self::reads(name, "It broke.");
        tool.output.error = Some(Failure {
            code,
            message: "It broke.".into(),
            retry_after: None,
            provider: None,
        });
        tool
    }

    pub(crate) fn ran(&self) -> Vec<Map<String, Value>> {
        self.ran.lock().unwrap().clone()
    }

    fn note(&self, what: &str) {
        self.trace.push(format!("{what} {}", self.name));
    }
}

impl Tool for TestTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: format!("The test tool {}.", self.name),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"},
                    "days": {"type": "integer"}
                },
                "required": ["city"],
                "additionalProperties": false
            }),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        self.note("effects");
        self.effects.clone().map(|declared| Effects {
            declared,
            subject: self.subject.clone(),
            prefix: self.prefix.clone(),
        })
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel) -> Output {
        self.note("run");
        self.cancelled.lock().unwrap().push(cancel.is_cancelled());
        self.ran.lock().unwrap().push(arguments.clone());
        if let Some(other) = self.after {
            self.trace.wait_for(&format!("done {other}"));
        }
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        self.note("done");
        self.output.clone()
    }

    fn bound(&self) -> Bound {
        self.bound
    }
}

/// The reasoning item a reasoning reply carries, as a provider sent it.
pub(crate) fn reasoning_item(thought: &str) -> Value {
    json!({"type": "reasoning", "encrypted_content": "gAAA-opaque", "summary": thought})
}

/// A driver's message.
pub(crate) fn message(text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: From {
            origin: Origin::Driver,
            command_id: CommandId(format!("c_{text}")),
        },
    }
}

/// An acknowledgement that ignores its answer. `fiber ask` uses one, and so
/// does a test that does not check the answer.
pub(crate) fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

/// A driver's prompt on the loop's inbox.
pub(crate) fn delivery(text: &str) -> Delivery {
    Delivery::Prompt(message(text), ignore())
}

/// A driver's steering message on the loop's inbox.
pub(crate) fn steer(text: &str) -> Delivery {
    Delivery::Steer(message(text), ignore())
}

/// Standing rules in memory: `read` returns what the test set.
pub(crate) struct FakeRules {
    rules: Mutex<Result<StandingRules, RulesError>>,
}

impl FakeRules {
    fn empty() -> Self {
        Self {
            rules: Mutex::new(Ok(StandingRules {
                global: Vec::new(),
                project: Vec::new(),
            })),
        }
    }

    /// What `read` returns from now on.
    pub(crate) fn set(&self, rules: StandingRules) {
        *self.rules.lock().unwrap() = Ok(rules);
    }

    /// `read` fails with `error` from now on.
    pub(crate) fn fail(&self, error: RulesError) {
        *self.rules.lock().unwrap() = Err(error);
    }
}

impl Rules for FakeRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        self.rules.lock().unwrap().clone()
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        // Only step 7 (#294) offers a rule to remember; nothing here does.
        Ok(())
    }
}

/// The scripted provider, sending `during` to the inbox as the first call is
/// made, so it arrives while that reply streams.
struct Seam {
    inner: Arc<ScriptedProvider>,
    /// Taken by the first call, so the loop sees the inbox close once the
    /// test drops its own sender.
    during: Mutex<Option<(Vec<Delivery>, Sender<Delivery>)>>,
}

impl Provider for Seam {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        if let Some((deliveries, inbox)) = self.during.lock().unwrap().take() {
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
        }
        self.inner.call(request)
    }
}

/// A session on a fresh log, its loop wired to a scripted provider.
pub(crate) struct Session {
    pub(crate) provider: Arc<ScriptedProvider>,
    pub(crate) log: Arc<Log>,
    pub(crate) dir: PathBuf,
    /// The workspace, an empty directory.
    pub(crate) workspace: PathBuf,
    /// Fiber home's `credentials/` directory.
    pub(crate) credentials: PathBuf,
    /// The standing rules the loop reads.
    pub(crate) rules: Arc<FakeRules>,
    pub(crate) inbox: Sender<Delivery>,
    lines: mpsc::Receiver<Envelope>,
    pub(crate) looped: Option<Loop>,
    _home: TempDir,
}

impl Session {
    /// Answers each model call with the next of `script`. The first model
    /// call sends `during` to the inbox, when there is one.
    pub(crate) fn new(script: Vec<Scripted>, during: Option<Message>) -> Self {
        Self::with_tools(script, during, Vec::new())
    }

    /// As [`Session::new`], with `tools` registered.
    pub(crate) fn with_tools(
        script: Vec<Scripted>,
        during: Option<Message>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::open(
            script,
            during
                .into_iter()
                .map(|message| Delivery::Steer(message, ignore()))
                .collect(),
            tools,
            unpriced(),
        )
    }

    /// As [`Session::new`], and the first model call sends `during`.
    pub(crate) fn injecting(script: Vec<Scripted>, during: Vec<Delivery>) -> Self {
        Self::open(script, during, Vec::new(), unpriced())
    }

    /// As [`Session::with_tools`], reaching `model`. `during` is sent, in
    /// order, when the first model call is made.
    pub(crate) fn open(
        script: Vec<Scripted>,
        during: Vec<Delivery>,
        tools: Vec<Arc<dyn Tool>>,
        model: Model,
    ) -> Self {
        let home = TempDir::new();
        let workspace = home.0.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let credentials = home.0.join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let id = SessionId("s_test".into());
        let log =
            Arc::new(Log::create(&home.0, id.clone(), fakes::clock::FakeClock::new()).unwrap());
        let mut watcher = log.watch();
        let (forward, lines) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(line)) = watcher.recv() {
                if forward.send(line).is_err() {
                    break;
                }
            }
        });
        let (inbox, rx) = mpsc::channel();
        let provider = Arc::new(ScriptedProvider::new(script));
        let seam = Seam {
            inner: Arc::clone(&provider),
            during: Mutex::new((!during.is_empty()).then(|| (during, inbox.clone()))),
        };
        let rules = Arc::new(FakeRules::empty());
        let looped = Loop::start(
            Arc::clone(&log),
            Arc::new(seam),
            model,
            "You are terse.".into(),
            rx,
            tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            r#loop::Permissions {
                workspace: workspace.display().to_string(),
                credentials: credentials.clone(),
                rules: rules.clone(),
            },
        )
        .unwrap();
        Self {
            provider,
            dir: home.0.join(&id.0),
            workspace,
            credentials,
            rules,
            log,
            inbox,
            lines,
            looped: Some(looped),
            _home: home,
        }
    }

    /// Whether a person can answer an approval.
    pub(crate) fn answerable(mut self, yes: bool) -> Self {
        self.looped = self.looped.take().map(|looped| looped.answerable(yes));
        self
    }

    /// Judges step 7's calls with a scripted reviewer answering `script`,
    /// with the default block limits. Returns the reviewer, so a test can
    /// read the requests it was given.
    pub(crate) fn reviewer(&mut self, script: Vec<Scripted>) -> Arc<ScriptedProvider> {
        self.reviewer_limits(script, BlockLimits::default())
    }

    /// As [`Session::reviewer`], with `limits`.
    pub(crate) fn reviewer_limits(
        &mut self,
        script: Vec<Scripted>,
        limits: BlockLimits,
    ) -> Arc<ScriptedProvider> {
        self.reviewer_priced(script, limits, None)
    }

    /// As [`Session::reviewer`], with `limits` and the reviewer's prices.
    pub(crate) fn reviewer_priced(
        &mut self,
        script: Vec<Scripted>,
        limits: BlockLimits,
        cost: Option<contract::provider::Cost>,
    ) -> Arc<ScriptedProvider> {
        let provider = Arc::new(ScriptedProvider::new(script));
        let looped = self.looped.take().unwrap().reviewer(
            Ok(Reviewer {
                provider: provider.clone(),
                model: Model {
                    reference: REVIEWER_MODEL.into(),
                    cost,
                    subscription: false,
                },
            }),
            limits,
        );
        self.looped = Some(looped);
        provider
    }

    /// Sends a person's answer to a pending approval.
    pub(crate) fn reply(&self, reply: contract::commands::Reply) {
        self.inbox.send(Delivery::Reply(reply, ignore())).unwrap();
    }

    /// Caps the session's billed spend at `usd` US dollars.
    pub(crate) fn budget(mut self, usd: Option<f64>) -> Self {
        self.looped = self.looped.take().map(|looped| looped.budget(usd));
        self
    }

    /// Runs one turn on its own thread, failing the test if it outlives
    /// [`DEADLINE`].
    pub(crate) fn turn(&mut self) -> Option<TurnOutcome> {
        let mut looped = self.looped.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = looped.turn().unwrap();
            done.send((looped, outcome)).unwrap();
        });
        let (looped, outcome) = finished
            .recv_timeout(DEADLINE)
            .expect("the turn ended in time");
        self.looped = Some(looped);
        outcome
    }

    /// Every request the loop built, in order.
    pub(crate) fn requests(&self) -> Vec<ModelRequest> {
        self.provider.requests()
    }

    /// Every line emitted since the last call, ephemeral ones included,
    /// through the next `turn_completed`.
    pub(crate) fn lines(&mut self) -> Vec<Envelope> {
        let mut lines = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(DEADLINE)
                .expect("a turn_completed line");
            let last = line.kind == "turn_completed";
            lines.push(line);
            if last {
                return lines;
            }
        }
    }
}

/// A model with no declared prices.
fn unpriced() -> Model {
    Model {
        reference: MODEL.into(),
        cost: None,
        subscription: false,
    }
}

/// The kinds of `lines`, in order.
pub(crate) fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

/// A directory removed when dropped.
pub(crate) struct TempDir(pub(crate) PathBuf, fakes::TempDir);

impl TempDir {
    pub(crate) fn new() -> Self {
        let held = fakes::TempDir::new("fiber-loop");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}
