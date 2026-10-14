//! The `review` jig's shared replay (`docs/testing.md`, "Jigs"): what the
//! loop's reviewer would be sent for a session directory's calls.
//!
//! The driver folds the source log's durable lines through the loop's own
//! [`render_reviewed`](r#loop::render_reviewed), snapshots the reviewer
//! input at each outer call's request, and replays each requested/started
//! pair through the loop's own
//! [`review`](r#loop::Loop::review) on a scratch loop, capturing both
//! stage requests. A reviewed call without logged started effects prints
//! one `unreplayable` line instead of invented input. At each completed
//! handoff with person messages the driver prints the selection request
//! and reads its answer, driving the loop's own
//! [`reviewer_handoff`](r#loop::Loop::reviewer_handoff); source
//! `reviewer_kept` lines are history, never a second selection. The source
//! directory is only read, never written.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use contract::events::{
    AskStep, DecidedBy, Event, HandoffCompleted, Outcome, ReviewerUse, ToolCallRequested,
    ToolCallStarted,
};
use contract::provider::{CallError, Delta, ModelCall, ModelRequest, Provider, Reply};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{DeclaredEffects, Failure};
use contract::tool::{Bound, Effects, EffectsError, Output, Tool};
use contract::{ActionId, ErrorCode, SessionId, TurnId};
use r#loop::{
    BlockLimits, Loop, Model, Permissions, PromptInputs, ReviewPrepared, Reviewed, Reviewer,
    Session, Shown, render_reviewed,
};
use serde_json::{Map, Value, json};

/// What the jig replays a session directory with.
pub(crate) struct ReviewArgs {
    /// The session directory to replay.
    pub dir: PathBuf,
    /// The person's reviewer notes, when `--notes` names a file.
    pub notes: Option<PathBuf>,
    /// Whether the selection-failure path keeps every person message.
    pub keep_all: bool,
}

/// Reads the jig's arguments: `[--notes FILE] [--keep-all] <session
/// directory>`.
pub(crate) fn parse_args(args: impl Iterator<Item = String>) -> Result<ReviewArgs, String> {
    let mut dir = None;
    let mut notes = None;
    let mut keep_all = false;
    let args = args.collect::<Vec<String>>();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        at += 1;
        if arg == "--notes" {
            match args.get(at) {
                Some(file) => {
                    at += 1;
                    notes = Some(PathBuf::from(file));
                }
                None => return Err("review: --notes needs a file".to_owned()),
            }
        } else if arg == "--keep-all" {
            keep_all = true;
        } else if arg.starts_with("--") {
            return Err(format!("review: unknown flag {arg}"));
        } else if dir.is_none() {
            dir = Some(PathBuf::from(arg));
        } else {
            return Err("review: one session directory, not two".to_owned());
        }
    }
    match dir {
        Some(dir) => Ok(ReviewArgs {
            dir,
            notes,
            keep_all,
        }),
        None => Err("usage: review [--notes FILE] [--keep-all] <session directory>".to_owned()),
    }
}

/// Runs the jig: prints one JSON line per replayable call, selection
/// request and unreplayable call, reading selection answers from `input`.
pub(crate) fn run(
    args: ReviewArgs,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<(), String> {
    replay(&args, input, output).map(|_| ())
}

/// As [`run`], keeping every captured request for the tests that compare
/// the replay against the native loop.
pub(crate) fn replay(
    args: &ReviewArgs,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<ReplayOutcome, String> {
    let notes = match &args.notes {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("review: cannot read notes {}: {error}", path.display()))?,
        None => String::new(),
    };
    let (workspace, chain) = load_chain(&args.dir)?;
    let mut driver = Driver::build(&notes, &workspace, input, output, args.keep_all)?;
    driver.drive(&chain)?;
    driver.finish()
}

/// What [`replay`] keeps: every captured request, in order.
#[derive(Debug)]
pub(crate) struct ReplayOutcome {
    /// Every request the scratch reviewer was sent, in order.
    pub requests: Vec<(RequestKind, ModelRequest)>,
}

/// Whether a captured request reviewed a call or selected handoff messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestKind {
    /// A review stage.
    Stage,
    /// A handoff selection.
    Selection,
}

/// One source log line in chain order, with the directory holding it.
struct Indexed {
    /// The directory whose log holds the line.
    dir: PathBuf,
    /// The line itself.
    envelope: contract::Envelope,
}

/// Reads the history chain's durable lines, oldest first, after checking
/// the log parses and begins with `session_started` (`docs/events.md`,
/// "The session directory"). Returns the source workspace and the chain.
fn load_chain(dir: &Path) -> Result<(String, Vec<Indexed>), String> {
    let segments = log::history(dir)
        .map_err(|error| format!("review: cannot read session {}: {error}", dir.display()))?;
    let mut chain = Vec::new();
    for segment in &segments {
        let lines = segment.lines(0).map_err(|error| {
            format!(
                "review: cannot read session {}: {error}",
                segment.dir.display()
            )
        })?;
        for envelope in lines {
            chain.push(Indexed {
                dir: segment.dir.clone(),
                envelope,
            });
        }
    }
    let Some(first) = chain.first() else {
        return Err(format!(
            "review: {} holds no session: the log is empty",
            dir.display()
        ));
    };
    if first.envelope.kind != "session_started" {
        return Err(format!(
            "review: {} holds no session: the log does not begin with session_started",
            first.dir.display()
        ));
    }
    let event = Event::from_envelope(&first.envelope).map_err(|error| {
        format!(
            "review: {} holds no session: its session_started does not parse: {error}",
            first.dir.display()
        )
    })?;
    if let Some(Event::SessionStarted(started)) = event {
        Ok((started.workspace, chain))
    } else {
        Err(format!(
            "review: {} holds no session: its session_started does not parse",
            first.dir.display()
        ))
    }
}

/// Standing rules that hold nothing: the replay judges nothing itself, it
/// only replays recorded calls through the reviewer.
struct EmptyRules;

impl Rules for EmptyRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules {
            global: Vec::new(),
            project: Vec::new(),
        })
    }

    fn remember(&self, _tool: &str, _prefix: &str, _session: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

/// The replay's inert tool: it carries the recorded declared effects and
/// is never classified or run. Only the recorded declaration and the
/// rewritten arguments enter a replayed request.
struct InertTool {
    /// What the source log's `tool_call_started` declared.
    declared: DeclaredEffects,
}

impl Tool for InertTool {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "replay".to_owned(),
            description: "The review jig's inert replay tool.".to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: self.declared.clone(),
            subject: None,
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn contract::tool::Cancel,
        _emit: &dyn contract::emit::Emit,
    ) -> Output {
        Output {
            content: Vec::new(),
            error: Some(Failure {
                code: ErrorCode::Other("replay-never-runs".to_owned()),
                message: "The review jig replays recorded effects; it never runs a tool."
                    .to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// The local failure a selection that never reached a reviewer carries: a
/// skipped (`--keep-all`) or unreadable (EOF) answer keeps every person
/// message through the existing fallback.
fn local_failure(message: &str) -> CallError {
    CallError::Failed {
        failure: Failure {
            code: ErrorCode::Other("review-selection".to_owned()),
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        },
        should_retry: None,
        usage: Box::new(fakes::unnamed_usage()),
    }
}

/// One scripted reviewer answer: `check`, then `allow`, alternating, so
/// both stage inputs are captured for every replayed call.
fn stage_answer(calls: u64) -> Reply {
    if calls.is_multiple_of(2) {
        fakes::reply("check")
    } else {
        fakes::reply("allow: fine")
    }
}

/// The replay's recording provider: it records every request, answers
/// review stages from the script, and serves handoff selections
/// interactively, printing each selection request before reading its
/// answer.
struct Seam {
    /// Every request it was given, in order.
    requests: Mutex<Vec<(RequestKind, ModelRequest)>>,
    /// How many stage requests it has answered.
    stages: Mutex<u64>,
    /// The handoff whose selection is being served, when one is.
    selecting: Mutex<Option<u64>>,
    /// The handoff already served: its answer is reused without further
    /// input, so a re-ask never prints a second request or reads again.
    served: Mutex<Option<u64>>,
    /// The served handoff's answer: text, or no answer on EOF.
    cached: Mutex<Option<String>>,
    /// Whether selections fail locally before exposing a request.
    keep_all: bool,
    /// The selection answers, read one handoff at a time.
    input: Mutex<Box<dyn BufRead + Send>>,
    /// Where selection requests and projected lines go.
    output: Mutex<Box<dyn Write + Send>>,
    /// The first selection write that failed, reported at the end.
    io_error: Mutex<Option<String>>,
}

impl Seam {
    /// Prints `line` as one JSON line. A write failure is stored for the
    /// driver to report, and the current selection fails locally.
    fn emit(&self, line: &str) -> Result<(), String> {
        let mut output = self.output.lock().unwrap_or_else(PoisonError::into_inner);
        match writeln!(output, "{line}") {
            Ok(()) => Ok(()),
            Err(error) => {
                let problem = format!("review: cannot write its output: {error}");
                *self.io_error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(problem.clone());
                Err(problem)
            }
        }
    }

    /// Serves a handoff selection: prints its request, flushes it, then
    /// reads one answer line. A repeat call for the same handoff reuses
    /// the cached answer with no further input.
    fn selection_call(&self, request: &ModelRequest, seq: u64) -> Box<dyn ModelCall> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((RequestKind::Selection, request.clone()));
        if self.keep_all {
            return Box::new(Call {
                result: Err(local_failure(
                    "the selection was skipped: --keep-all keeps every person message",
                )),
            });
        }
        if *self.served.lock().unwrap_or_else(PoisonError::into_inner) == Some(seq) {
            let cached = self
                .cached
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            return Box::new(Call {
                result: match cached {
                    Some(answer) => Ok(fakes::reply(&answer)),
                    None => Err(local_failure(
                        "the selection answer ended: it keeps every person message",
                    )),
                },
            });
        }
        let line = json!({
            "handoff": seq,
            "system_prompt": request.system_prompt,
            "messages": request.conversation,
        });
        let text = serde_json::to_string(&line).unwrap_or_else(|_| "{}".to_owned());
        if self.emit(&text).is_err() {
            return Box::new(Call {
                result: Err(local_failure(
                    "its output failed: it keeps every person message",
                )),
            });
        }
        {
            let mut output = self.output.lock().unwrap_or_else(PoisonError::into_inner);
            if output.flush().is_err() {
                let problem = "review: cannot write its output".to_owned();
                *self.io_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(problem);
                return Box::new(Call {
                    result: Err(local_failure(
                        "its output failed: it keeps every person message",
                    )),
                });
            }
        }
        let mut answer = String::new();
        let read = self
            .input
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read_line(&mut answer);
        if let Ok(bytes) = read
            && bytes > 0
        {
            let answer = answer.trim_end().to_owned();
            *self.served.lock().unwrap_or_else(PoisonError::into_inner) = Some(seq);
            *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer.clone());
            return Box::new(Call {
                result: Ok(fakes::reply(&answer)),
            });
        }
        *self.served.lock().unwrap_or_else(PoisonError::into_inner) = Some(seq);
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Box::new(Call {
            result: Err(local_failure(
                "the selection answer ended: it keeps every person message",
            )),
        })
    }
}

/// One answered reviewer call: the scripted reply, or the local failure.
struct Call {
    /// How the call ends.
    result: Result<Reply, CallError>,
}

impl ModelCall for Call {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        self.result.clone()
    }

    fn cancel(&self) {}
}

impl Provider for Seam {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let selecting = *self
            .selecting
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(seq) = selecting {
            return self.selection_call(request, seq);
        }
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((RequestKind::Stage, request.clone()));
        let mut stages = self.stages.lock().unwrap_or_else(PoisonError::into_inner);
        let answer = stage_answer(*stages);
        *stages += 1;
        Box::new(Call { result: Ok(answer) })
    }

    fn wire_tools(&self, tools: &[contract::provider::ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        None
    }

    fn warms(&self, _request: &ModelRequest) -> bool {
        false
    }
}

/// One outer call the driver indexed: its request, the reviewer input at
/// that moment, and the start that declared its effects, when one logged.
struct Pending {
    /// The call's action.
    id: ActionId,
    /// The requested call.
    call: ToolCallRequested,
    /// The turn that requested it.
    turn: Option<TurnId>,
    /// The reviewer input through this call's own item: later person
    /// messages and sibling calls never enter its replay.
    snapshot: Vec<Reviewed>,
    /// What the call declared, once its start logs.
    started: Option<ToolCallStarted>,
    /// The turn that started it.
    start_turn: Option<TurnId>,
}

/// The replay driver: the scratch loop, the source fold, and the index of
/// the calls it replays.
struct Driver {
    /// The scratch loop, in its own temporary directory.
    looped: Loop,
    /// The recording seam answering its reviewer.
    seam: Arc<Seam>,
    /// The scratch session's directory, for reading back selections.
    scratch: PathBuf,
    /// The source fold: what the reviewer is shown, line by line.
    reviewed: Vec<Reviewed>,
    /// Every outer call in source request order.
    pending: Vec<Pending>,
    /// Each pending call's place, by action id.
    index: HashMap<String, usize>,
    /// The actions with recorded reviewer attribution.
    attributed: HashSet<String>,
    /// The temporary home, kept alive for the replay.
    _home: fakes::TempDir,
    /// The inbox sender, kept so the scratch inbox never closes.
    _inbox: std::sync::mpsc::Sender<contract::inbox::Delivery>,
}

impl Driver {
    /// Builds the scratch loop: its own temporary directory and fake
    /// clock, the person's notes, and an unbounded reviewer window, so
    /// `--keep-all` really keeps every earlier person message. The source
    /// workspace labels its effects items, never the scratch directory.
    fn build(
        notes: &str,
        workspace: &str,
        input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
        keep_all: bool,
    ) -> Result<Self, String> {
        let home = fakes::TempDir::new("fiber-review");
        let clock = fakes::clock::FakeClock::new();
        let log = log::Log::create(home.path(), SessionId("s_replay".to_owned()), clock.clone())
            .map_err(|error| format!("review: cannot start its replay: {error}"))?;
        let scratch = log.dir().to_path_buf();
        let seam = Arc::new(Seam {
            requests: Mutex::new(Vec::new()),
            stages: Mutex::new(0),
            selecting: Mutex::new(None),
            served: Mutex::new(None),
            cached: Mutex::new(None),
            keep_all,
            input: Mutex::new(Box::new(input)),
            output: Mutex::new(Box::new(output)),
            io_error: Mutex::new(None),
        });
        let session_log = scratch.join("events.jsonl").display().to_string();
        let clock: Arc<dyn contract::clock::Clock> = clock;
        let prompt = PromptInputs::new(
            home.path().to_path_buf(),
            "/bin/sh".to_owned(),
            session_log,
            clock,
            u64::MAX,
        );
        let empty = Arc::new(EmptyRules);
        let (inbox, rx) = std::sync::mpsc::channel();
        let mut looped = Loop::start(
            Session {
                log: Arc::new(log),
                provider: seam.clone(),
                model: Model {
                    reference: "fake/session".to_owned(),
                    cost: None,
                    subscription: false,
                },
                prompt,
                inbox: rx,
                tools: Vec::new(),
                permissions: Permissions {
                    workspace: scratch.display().to_string(),
                    credentials: home.path().join("credentials"),
                    credential_files: Vec::new(),
                    rules: empty,
                },
            },
            None,
        )
        .map_err(|error| format!("review: cannot start its replay: {error}"))?
        .reviewer(
            Ok(Reviewer {
                provider: seam.clone(),
                model: Model {
                    reference: "fake/reviewer".to_owned(),
                    cost: None,
                    subscription: false,
                },
                thinking_levels: Vec::new(),
                cache_lifetime: contract::events::CacheLifetime::OneHour,
                context_window: u64::MAX,
            }),
            BlockLimits::default(),
        )
        .reviewer_notes(notes.to_owned());
        looped.workspace_label = workspace.to_owned();
        Ok(Self {
            looped,
            seam,
            scratch,
            reviewed: Vec::new(),
            pending: Vec::new(),
            index: HashMap::new(),
            attributed: HashSet::new(),
            _home: home,
            _inbox: inbox,
        })
    }

    /// Folds the chain and replays it: review lines print where their
    /// calls started, selections where their handoffs completed, and
    /// unreplayable lines at the end in source request order.
    fn drive(&mut self, chain: &[Indexed]) -> Result<(), String> {
        for (position, item) in chain.iter().enumerate() {
            let envelope = &item.envelope;
            let event = Event::from_envelope(envelope).map_err(|error| {
                format!(
                    "review: {} {}: a {} payload that does not parse: {error}",
                    item.dir.display(),
                    describe(envelope, position),
                    envelope.kind,
                )
            })?;
            let Some(event) = event else {
                // A consumer skips any kind it does not recognise.
                continue;
            };
            if matches!(event, Event::ReviewerKept(_)) {
                // The driver owns selection: a source record never
                // overwrites the selection its own handoffs make.
                continue;
            }
            if let Event::HandoffCompleted(done) = event {
                if done.outcome == Outcome::Completed {
                    self.completed_handoff(envelope, &done)?;
                } else {
                    render_reviewed(
                        &mut self.reviewed,
                        &Event::HandoffCompleted(done),
                        envelope.action_id.as_ref(),
                        envelope.seq,
                    );
                }
                continue;
            }
            render_reviewed(
                &mut self.reviewed,
                &event,
                envelope.action_id.as_ref(),
                envelope.seq,
            );
            self.index_event(envelope, &event)?;
        }
        for pending in &self.pending {
            if pending.started.is_none() && self.attributed.contains(&pending.id.0) {
                let line = json!({
                    "id": pending.id.0,
                    "unreplayable": "declared effects not logged",
                });
                self.emit(&line)?;
            }
        }
        Ok(())
    }

    /// Indexes one rendered event: requests snapshot the reviewer input,
    /// starts replay their calls, and review attribution marks denied
    /// calls that never started.
    fn index_event(&mut self, envelope: &contract::Envelope, event: &Event) -> Result<(), String> {
        if let Event::ToolCallRequested(call) = event {
            // Inner and provider-hosted calls are history, never reviewed
            // targets of their own.
            if call.ran_by.is_none()
                && call.provider_item.is_none()
                && let Some(action) = envelope.action_id.clone()
                && !self.index.contains_key(&action.0)
            {
                self.index.insert(action.0.clone(), self.pending.len());
                self.pending.push(Pending {
                    id: action,
                    call: call.clone(),
                    turn: envelope.turn_id.clone(),
                    snapshot: self.reviewed.clone(),
                    started: None,
                    start_turn: None,
                });
            }
            return Ok(());
        }
        if let Event::ToolCallStarted(started) = event
            && let Some(action) = envelope.action_id.clone()
            && let Some(at) = self.index.get(&action.0).copied()
            && self
                .pending
                .get(at)
                .map(|pending| pending.started.is_none())
                .unwrap_or(false)
        {
            if let Some(pending) = self.pending.get_mut(at) {
                pending.started = Some(started.clone());
                pending.start_turn = envelope.turn_id.clone();
            }
            self.replay_call(envelope, at)?;
            return Ok(());
        } else if matches!(event, Event::ToolCallStarted(_)) {
            return Ok(());
        }
        if let Event::PermissionRequested(request) = event {
            if matches!(request.step, AskStep::Review { .. })
                && let Some(action) = envelope.action_id.clone()
            {
                self.attributed.insert(action.0);
            }
            return Ok(());
        }
        if let Event::PermissionResolved(resolved) = event {
            if (resolved.reviewer.is_some()
                || matches!(
                    resolved.decided_by,
                    DecidedBy::Reviewer | DecidedBy::NoReviewer
                ))
                && let Some(action) = envelope.action_id.clone()
            {
                self.attributed.insert(action.0);
            }
            return Ok(());
        }
        if let Event::UsageRecorded(recorded) = event
            && let Some(use_of) = recorded.reviewer.clone()
            && let ReviewerUse::Stage1 { action_id } | ReviewerUse::Stage2 { action_id } = use_of
        {
            self.attributed.insert(action_id.0);
        }
        Ok(())
    }

    /// Replays one requested/started pair through the loop's own reviewer
    /// and prints both captured stage requests as one JSON line.
    fn replay_call(&mut self, envelope: &contract::Envelope, at: usize) -> Result<(), String> {
        let Some(pending) = self.pending.get(at) else {
            return Err("review: its call index disagrees with its calls".to_owned());
        };
        let Some(started) = pending.started.clone() else {
            return Err("review: its call index disagrees with its calls".to_owned());
        };
        let arguments = match started.arguments.clone() {
            Some(rewritten) => rewritten,
            None => match pending.call.arguments.clone() {
                Value::Object(arguments) => arguments,
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_) => Map::new(),
            },
        };
        let rewritten = match started.arguments.clone() {
            Some(_) => Some(started.changed_by.clone().unwrap_or_default()),
            None => None,
        };
        let prepared = ReviewPrepared {
            tool: Arc::new(InertTool {
                declared: started.declared.clone(),
            }),
            arguments,
            effects: Effects {
                declared: started.declared.clone(),
                subject: None,
                prefix: None,
                always_reviewed: false,
            },
            rewritten,
        };
        let id = pending.id.clone();
        let call = pending.call.clone();
        let snapshot = pending.snapshot.clone();
        let turn = pending
            .start_turn
            .clone()
            .or(pending.turn.clone())
            .unwrap_or(TurnId("t_replay".to_owned()));
        self.looped.reviewed = snapshot;
        let before = self
            .seam
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let _decision = self
            .looped
            .review(&call, &id, &turn, prepared)
            .map_err(|error| {
                format!(
                    "review: {}: cannot replay call {}: {error}",
                    describe(envelope, at),
                    id.0,
                )
            })?;
        let requests = self
            .seam
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let stages: Vec<ModelRequest> = requests
            .iter()
            .skip(before)
            .filter_map(|(kind, request)| {
                if *kind == RequestKind::Stage {
                    Some(request.clone())
                } else {
                    None
                }
            })
            .collect();
        let Some(first) = stages.first() else {
            return Err(format!(
                "review: {}: its reviewer answered no stage request for call {}",
                describe(envelope, at),
                id.0,
            ));
        };
        let Some(second) = stages.get(1) else {
            return Err(format!(
                "review: {}: its reviewer answered one stage request for call {}, not two",
                describe(envelope, at),
                id.0,
            ));
        };
        if stages.len() != 2 {
            return Err(format!(
                "review: {}: its reviewer answered {} stage requests for call {}, not two",
                describe(envelope, at),
                stages.len(),
                id.0,
            ));
        }
        let line = json!({
            "id": id.0,
            "stages": [
                {"stage": 1, "system_prompt": first.system_prompt, "messages": first.conversation},
                {"stage": 2, "system_prompt": second.system_prompt, "messages": second.conversation},
            ],
        });
        self.emit(&line)
    }

    /// Replays one completed handoff: renders its boundary, asks the
    /// selection through the loop's own handoff path, and folds the kept
    /// messages the answer selected. With no person message the loop
    /// records an empty selection itself: no request prints and no answer
    /// is read. `--keep-all` fails the selection locally before any
    /// request prints.
    fn completed_handoff(
        &mut self,
        envelope: &contract::Envelope,
        done: &HandoffCompleted,
    ) -> Result<(), String> {
        render_reviewed(
            &mut self.reviewed,
            &Event::HandoffCompleted(done.clone()),
            envelope.action_id.as_ref(),
            envelope.seq,
        );
        let turn = envelope
            .turn_id
            .clone()
            .unwrap_or(TurnId("t_replay".to_owned()));
        self.looped.reviewed.clone_from(&self.reviewed);
        self.looped
            .write(
                &Event::HandoffCompleted(done.clone()),
                Some(&turn),
                envelope.action_id.as_ref(),
            )
            .map_err(|error| format!("review: cannot replay its handoff: {error}"))?;
        if self
            .reviewed
            .iter()
            .any(|item| matches!(item.shown, Shown::Person(_)))
        {
            let seq = envelope.seq.map(|seq| seq.0).unwrap_or(0);
            *self
                .seam
                .selecting
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(seq);
            let answered = self.looped.reviewer_handoff(&turn);
            *self
                .seam
                .selecting
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
            answered.map_err(|error| format!("review: cannot replay its handoff: {error}"))?;
            if let Some(error) = self
                .seam
                .io_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                return Err(error);
            }
        } else {
            self.looped
                .reviewer_handoff(&turn)
                .map_err(|error| format!("review: cannot replay its handoff: {error}"))?;
        }
        self.fold_kept()
    }

    /// Folds the scratch loop's latest `reviewer_kept` into the source
    /// fold, so repeated handoffs select from the previously kept
    /// messages, never the whole earlier transcript.
    fn fold_kept(&mut self) -> Result<(), String> {
        let lines = log::read(&self.scratch).map_err(|error| format!("review: {error}"))?;
        let mut kept = None;
        for line in &lines {
            if line.kind == "reviewer_kept" {
                kept = Some(line.clone());
            }
        }
        let Some(kept) = kept else {
            return Err("review: its replay wrote no reviewer_kept".to_owned());
        };
        let event = Event::from_envelope(&kept)
            .map_err(|error| format!("review: its replay wrote no reviewer_kept: {error}"))?;
        if let Some(Event::ReviewerKept(selected)) = event {
            render_reviewed(
                &mut self.reviewed,
                &Event::ReviewerKept(selected),
                None,
                kept.seq,
            );
            Ok(())
        } else {
            Err("review: its replay wrote no reviewer_kept".to_owned())
        }
    }

    /// Prints one projected line.
    fn emit(&self, line: &Value) -> Result<(), String> {
        let text = serde_json::to_string(line)
            .map_err(|error| format!("review: cannot print its replay: {error}"))?;
        self.seam.emit(&text)
    }

    /// Flushes the output and returns every captured request.
    fn finish(self) -> Result<ReplayOutcome, String> {
        self.seam
            .output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
            .map_err(|error| format!("review: cannot write its output: {error}"))?;
        if let Some(error) = self
            .seam
            .io_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Err(error);
        }
        let requests = self
            .seam
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Ok(ReplayOutcome { requests })
    }
}

/// Names a source line for diagnostics: its `seq`, else its position.
fn describe(envelope: &contract::Envelope, position: usize) -> String {
    match envelope.seq {
        Some(seq) => format!("seq {}", seq.0),
        None => format!("line {}", position + 1),
    }
}
