//! Steering paths: definition, effects, targets, rejections, backoff,
//! commit races and cancel.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::Event;
use contract::shapes::Failure;
use contract::tool::{Output, Tool};
use contract::{ErrorCode, SessionId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Deadline, Recorder, TempDir};
use serde_json::{Map, Value, json};

use super::{Attempt, DelegateMessage, Launch, Steer, Steered};
use crate::registry::Registry;
use crate::support::{delegate_finished, delegate_started, ended_ok};

/// The stop bound and closing wait the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// One steer call, as the tool made it.
struct SteerCall {
    session: SessionId,
    command: String,
    text: String,
}

/// What a scripted steer answers, after an optional hold and commit.
struct SteerReply {
    /// What `commit` answers; false ends `Withheld` with nothing written.
    commit: bool,
    /// Held before `commit`, standing in for a blocked connection.
    hold_before: Option<mpsc::Receiver<()>>,
    /// Held after `commit`, standing in for a blocked write.
    hold: Option<mpsc::Receiver<()>>,
    /// Whether `on_queued` runs before the answer.
    queued: bool,
    answer: SteerAnswer,
}

enum SteerAnswer {
    Accepted,
    Rejected(ErrorCode, String),
    Gone,
}

impl SteerReply {
    fn accept() -> Self {
        Self {
            commit: true,
            hold_before: None,
            hold: None,
            queued: true,
            answer: SteerAnswer::Accepted,
        }
    }

    fn gone() -> Self {
        Self {
            commit: true,
            hold_before: None,
            hold: None,
            queued: false,
            answer: SteerAnswer::Gone,
        }
    }

    fn rejected(code: ErrorCode, message: &str) -> Self {
        Self {
            commit: true,
            hold_before: None,
            hold: None,
            queued: false,
            answer: SteerAnswer::Rejected(code, message.into()),
        }
    }
}

/// A scripted `Steer`: records every call, answers from the queue.
struct Script {
    calls: Mutex<Vec<SteerCall>>,
    /// Notified on every recorded call.
    called: Condvar,
    replies: Mutex<VecDeque<SteerReply>>,
    commits: Mutex<Vec<bool>>,
    /// Notified on every recorded commit decision.
    committed: Condvar,
    /// Entered holds, counting each.
    holds: Mutex<usize>,
    held: Condvar,
}

impl Script {
    fn scripted(replies: Vec<SteerReply>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            called: Condvar::new(),
            replies: Mutex::new(replies.into()),
            commits: Mutex::new(Vec::new()),
            committed: Condvar::new(),
            holds: Mutex::new(0),
            held: Condvar::new(),
        })
    }

    fn steer(
        self: &Arc<Self>,
        session: &SessionId,
        command: &str,
        text: &str,
        commit: &mut dyn FnMut() -> bool,
        on_queued: &mut dyn FnMut(),
    ) -> Steered {
        self.calls.lock().unwrap().push(SteerCall {
            session: session.clone(),
            command: command.into(),
            text: text.into(),
        });
        self.called.notify_all();
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted steer answer");
        if let Some(hold) = reply.hold_before {
            *self.holds.lock().unwrap() += 1;
            self.held.notify_all();
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&hold)
                .expect("the hold released the connection");
        }
        if !reply.commit {
            // The commit is refused before the tool's own runs, as a
            // cancelled or stale gate refuses it.
            self.commits.lock().unwrap().push(false);
            self.committed.notify_all();
            return Steered::Withheld;
        }
        if !commit() {
            self.commits.lock().unwrap().push(false);
            self.committed.notify_all();
            return Steered::Withheld;
        }
        self.commits.lock().unwrap().push(true);
        self.committed.notify_all();
        if let Some(hold) = reply.hold {
            *self.holds.lock().unwrap() += 1;
            self.held.notify_all();
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&hold)
                .expect("the hold released the write");
        }
        if reply.queued {
            on_queued();
        }
        match reply.answer {
            SteerAnswer::Accepted => Steered::Accepted,
            SteerAnswer::Rejected(code, message) => Steered::Rejected(Failure {
                code,
                message,
                retry_after_ms: None,
                provider: None,
            }),
            SteerAnswer::Gone => Steered::Gone,
        }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| (call.command.clone(), call.text.clone()))
            .collect()
    }

    fn sessions(&self) -> Vec<SessionId> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.session.clone())
            .collect()
    }

    /// Waits, at most `within` of real time, until the steer entered a
    /// hold `n` times. True once it has; false at the deadline.
    fn await_holds(&self, n: usize, within: Duration) -> bool {
        let (holds, _) = self
            .held
            .wait_timeout_while(self.holds.lock().unwrap(), within, |holds| *holds < n)
            .unwrap();
        *holds >= n
    }

    /// Waits, at most `within` of real time, until the steer recorded `n`
    /// commit decisions. True once it has; false at the deadline.
    fn await_commits(&self, n: usize, within: Duration) -> bool {
        let (commits, _) = self
            .committed
            .wait_timeout_while(self.commits.lock().unwrap(), within, |commits| {
                commits.len() < n
            })
            .unwrap();
        commits.len() >= n
    }

    /// Waits, at most `within` of real time, until the steer ran `n`
    /// times. True once it has; false at the deadline.
    fn await_calls(&self, n: usize, within: Duration) -> bool {
        let (calls, _) = self
            .called
            .wait_timeout_while(self.calls.lock().unwrap(), within, |calls| calls.len() < n)
            .unwrap();
        calls.len() >= n
    }
}

/// The tool over fakes: a scripted steer, a fixed boundary answer, and a
/// launch that must never run in the steering tests.
struct Rig {
    _dir: TempDir,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    tool: Arc<DelegateMessage>,
    script: Arc<Script>,
    launched: Arc<AtomicUsize>,
}

fn rig(replies: Vec<SteerReply>) -> Rig {
    let dir = TempDir::new("fiber-delegate-message");
    let clock = FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let script = Script::scripted(replies);
    let steered = Arc::clone(&script);
    let steer: Steer = Arc::new(move |session, command, text, commit, on_queued| {
        steered.steer(session, command, text, commit, on_queued)
    });
    let launched = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&launched);
    let launch: Launch = Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        panic!("no steering test launches");
    });
    let tool = Arc::new(DelegateMessage {
        finish: Arc::new(|end| contract::events::FinishedWorktree {
            worktree: contract::shapes::Worktree {
                path: end.path.to_string_lossy().into_owned(),
                branch: end.branch,
                repository: None,
            },
            dirty: false,
        }),
        depth: 0,
        registry: Arc::clone(&registry),
        parent: SessionId("s_parent".into()),
        clock: Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        bound: BOUND,
        cap: 1024,
        launch,
        watch: Arc::new(|_, _, _| panic!("no steering test watches")),
        steer,
        last_seq: Arc::new(|_| panic!("no steering test reads the log")),
    });
    Rig {
        _dir: dir,
        registry,
        clock,
        tool,
        script,
        launched,
    }
}

/// Opens a running delegate for `description`.
fn open_delegate(rig: &Rig, description: &str) -> (contract::JobId, crate::registry::Unreported) {
    let id = crate::registry::mint_job_id();
    let (_started, finish) = rig.registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        description.into(),
        "/sessions/s_x/events.jsonl".into(),
        contract::jobs::Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    (id, finish)
}

fn args(id: &str, message: &str) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("id".into(), json!(id));
    args.insert("message".into(), json!(message));
    args
}

fn content(output: &Output) -> &str {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn code_of(output: &Output) -> ErrorCode {
    output.error.as_ref().expect("the call failed").code.clone()
}

#[test]
fn depth_refuses_before_steering_or_resuming_with_the_same_definition() {
    for depth in [2, 3] {
        let mut rig = rig(vec![]);
        let (id, _finish) = open_delegate(&rig, "running");
        let definition = serde_json::to_value(rig.tool.definition()).unwrap();
        Arc::get_mut(&mut rig.tool).unwrap().depth = depth;
        assert_eq!(
            serde_json::to_value(rig.tool.definition()).unwrap(),
            definition
        );
        for call in [args(&id.0, "hello"), Map::new()] {
            let tool = Arc::clone(&rig.tool);
            let output = fakes::within(
                "depth refusal",
                Deadline::after(MUST_SUCCEED_WITHIN),
                move || tool.run(&call, &CancelToken::new(), &Recorder::default()),
            );
            assert_eq!(code_of(&output), ErrorCode::DepthExceeded);
            assert!(content(&output).contains("`delegate_message`"));
            assert!(rig.script.calls().is_empty());
            assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
        }
    }
    let mut rig = rig(vec![]);
    Arc::get_mut(&mut rig.tool).unwrap().depth = 1;
    let output = rig.tool.run(
        &args("j_unknown", "hello"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code_of(&output), ErrorCode::InvalidArguments);
}

#[test]
fn the_definition_names_id_and_message_and_nothing_else() {
    let rig = rig(vec![]);
    let definition = rig.tool.definition();
    assert_eq!(definition.name, "delegate_message");
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The delegate's job id, from its receipt."
                },
                "message": {
                    "type": "string",
                    "description": "What to tell it."
                }
            },
            "required": ["id", "message"],
            "additionalProperties": false
        })
    );
    assert!(!definition.deferred);
}

#[test]
fn effects_declare_executes_and_always_reviewed() {
    let rig = rig(vec![]);
    let effects = rig.tool.effects(&args("j_x", "hi")).unwrap();
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
    assert!(!effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
    assert!(effects.always_reviewed);
    // Every branch of the argument check fails, from `effects` and `run`.
    for arguments in [
        Map::new(),
        args("j_x", "hi")
            .into_iter()
            .filter(|(name, _)| name != "message")
            .collect(),
    ] {
        assert!(rig.tool.effects(&arguments).is_err());
        assert!(
            rig.tool
                .run(&arguments, &CancelToken::new(), &Recorder::default())
                .error
                .is_some()
        );
    }
    let mut wrong = args("j_x", "hi");
    wrong.insert("message".into(), json!(7));
    assert!(rig.tool.effects(&wrong).is_err());
    assert!(
        rig.tool
            .run(&wrong, &CancelToken::new(), &Recorder::default())
            .error
            .is_some()
    );
}

#[test]
fn an_unknown_id_fails_as_jobs_does() {
    let rig = rig(vec![]);
    let output = rig.tool.run(
        &args("j_x", "hi"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code_of(&output), ErrorCode::InvalidArguments);
    assert_eq!(content(&output), "No job has id `j_x`.\n");
    assert!(rig.script.calls().is_empty(), "steer never ran");
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_job_that_is_not_a_delegate_fails() {
    let rig = rig(vec![]);
    let opened = rig
        .registry
        .open(contract::jobs::Opening {
            tool: "shell".into(),
            description: "npm test".into(),
            stop: contract::jobs::Stop(Box::new(|| {})),
            lines: false,
            input: None,
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let output = rig
        .tool
        .run(&args(&id, "hi"), &CancelToken::new(), &Recorder::default());
    assert_eq!(code_of(&output), ErrorCode::InvalidArguments);
    assert_eq!(content(&output), format!("Job `{id}` is not a delegate.\n"));
    assert!(rig.script.calls().is_empty());
    drop(opened.end);
}

#[test]
fn a_steered_message_reports_steered_with_one_delta() {
    let rig = rig(vec![SteerReply::accept()]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let emit = Recorder::default();
    let output = rig
        .tool
        .run(&args(&id.0, "check this"), &CancelToken::new(), &emit);
    assert!(output.error.is_none());
    assert_eq!(
        content(&output),
        format!(
            "Steered delegate {}: the message joins its turn at its next step boundary.",
            id.0
        )
    );
    assert!(output.jobs.is_empty());
    assert!(output.settle.is_none());
    let calls = rig.script.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "check this");
    assert_eq!(
        rig.script.sessions(),
        vec![delegate_started(&id.0).delegate_session_id]
    );
    assert_eq!(rig.script.commits.lock().unwrap().as_slice(), [true]);
    let deltas: Vec<_> = emit
        .events()
        .into_iter()
        .filter(|event| matches!(event, Event::ToolCallDelta(_)))
        .collect();
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        emit.text(),
        format!(
            "Queued for delegate {}; it joins at the delegate's next step boundary.",
            id.0
        )
    );
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_refused_message_keeps_a_tool_call_code() {
    for code in [ErrorCode::MessageRefused, ErrorCode::HookFailed] {
        let rig = rig(vec![SteerReply::rejected(code.clone(), "nope")]);
        let (id, _finish) = open_delegate(&rig, "scan");
        let output = rig.tool.run(
            &args(&id.0, "hi"),
            &CancelToken::new(),
            &Recorder::default(),
        );
        assert_eq!(code_of(&output), code);
        assert_eq!(
            content(&output),
            format!("Delegate {} refused the message: nope\n", id.0)
        );
        assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn a_refused_message_with_a_driver_code_fails_tool_error_naming_it() {
    for (code, name) in [
        (ErrorCode::SessionHeld, "session_held"),
        (ErrorCode::NotSubscribed, "not_subscribed"),
        (ErrorCode::Other("x".into()), "x"),
    ] {
        let rig = rig(vec![SteerReply::rejected(code, "busy")]);
        let (id, _finish) = open_delegate(&rig, "scan");
        let output = rig.tool.run(
            &args(&id.0, "hi"),
            &CancelToken::new(),
            &Recorder::default(),
        );
        assert_eq!(code_of(&output), ErrorCode::ToolError);
        assert_eq!(
            content(&output),
            format!("Delegate {} refused the message ({name}): busy\n", id.0)
        );
        assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn an_empty_message_fails_before_any_lookup() {
    for message in ["", " ", "\n\t ", "\u{a0}"] {
        for setup in ["ended", "running", "unknown"] {
            let rig = rig(vec![]);
            let id = match setup {
                "ended" => {
                    let (id, finish) = open_delegate(&rig, "scan");
                    end_run(finish, &id);
                    id.0.clone()
                }
                "running" => open_delegate(&rig, "scan").0.0.clone(),
                _ => "j_missing".to_owned(),
            };
            let output = rig.tool.run(
                &args(&id, message),
                &CancelToken::new(),
                &Recorder::default(),
            );
            assert_eq!(
                code_of(&output),
                ErrorCode::InvalidArguments,
                "for {message:?}"
            );
            assert_eq!(content(&output), "`message` must not be empty.\n");
            assert!(rig.script.calls().is_empty(), "steer never ran");
            assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
        }
    }
    // At the edges a message is passed whole, whitespace included.
    for message in ["x", " x "] {
        let rig = rig(vec![SteerReply::accept()]);
        let (id, _finish) = open_delegate(&rig, "scan");
        let output = rig.tool.run(
            &args(&id.0, message),
            &CancelToken::new(),
            &Recorder::default(),
        );
        assert!(output.error.is_none(), "for {message:?}");
        assert_eq!(rig.script.calls()[0].1, message);
    }
}

fn end_run(finish: crate::registry::Unreported, id: &contract::JobId) {
    finish.report(ended_ok(&id.0), Some(delegate_finished(&id.0, "Done.")));
}

/// Runs the call on its own thread, answering on `tx`.
fn call_async(
    tool: &Arc<DelegateMessage>,
    id: &str,
    message: &str,
    cancel: CancelToken,
    emit: Arc<Recorder>,
) -> (mpsc::Receiver<Output>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let tool = Arc::clone(tool);
    let args = args(id, message);
    let handle = thread::spawn(move || {
        let output = tool.run(&args, &cancel, emit.as_ref());
        let _sent = tx.send(output);
    });
    (rx, handle)
}

/// The call's answer, failing at the wall deadline.
fn answered(rx: &mpsc::Receiver<Output>) -> Output {
    fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(rx)
        .expect("the call answered")
}

#[test]
fn gone_backs_off_doubling_to_a_capped_second() {
    let mut replies: Vec<SteerReply> = (0..7).map(|_| SteerReply::gone()).collect();
    replies.push(SteerReply::accept());
    let rig = rig(replies);
    let (id, _finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    assert!(
        rig.script.await_calls(1, MUST_SUCCEED_WITHIN),
        "the first attempt ran"
    );
    let mut at = rig.clock.now();
    // Five failures wait 50, 100, 200, 400 and 800 ms, then the cap: a
    // sixth and seventh failure wait 1 000 ms each.
    for (calls, wait) in [50, 100, 200, 400, 800, 1_000, 1_000]
        .into_iter()
        .enumerate()
    {
        let until = at + Duration::from_millis(wait);
        assert!(
            rig.clock
                .await_parked(until, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
            "the call parks its backoff"
        );
        let mark = rig.clock.advance_when_parked(
            Some(until),
            None,
            Duration::from_millis(wait - 1),
            &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
        );
        assert_eq!(
            rig.script.calls().len(),
            calls + 1,
            "no attempt before its backoff"
        );
        assert!(
            rig.clock.await_parked_since(
                &mark,
                Some(until),
                fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            ),
            "the call re-parks its backoff"
        );
        rig.clock.advance_when_parked(
            Some(until),
            None,
            Duration::from_millis(1),
            &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
        );
        assert!(
            rig.script.await_calls(calls + 2, MUST_SUCCEED_WITHIN),
            "the next attempt ran at its backoff"
        );
        at = until;
    }
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(
        content(&output),
        format!(
            "Steered delegate {}: the message joins its turn at its next step boundary.",
            id.0
        )
    );
    // One command id for the call, on every attempt.
    let calls = rig.script.calls();
    assert_eq!(calls.len(), 8);
    assert!(calls.iter().all(|(command, _)| command == &calls[0].0));
    assert!(calls[0].0.starts_with("c_"));
    assert_eq!(
        rig.script.commits.lock().unwrap().as_slice(),
        [true; 8],
        "every attempt committed"
    );
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_committed_gone_resends_with_the_same_id() {
    let rig = rig(vec![SteerReply::gone(), SteerReply::accept()]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    let backoff = rig.clock.now() + Duration::from_millis(50);
    assert!(
        rig.clock
            .await_parked(backoff, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its backoff"
    );
    rig.clock.advance_when_parked(
        Some(backoff),
        None,
        Duration::from_millis(50),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert!(content(&output).starts_with(&format!("Steered delegate {}", id.0)));
    let calls = rig.script.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, calls[1].0, "the resend keeps the command id");
    assert_eq!(rig.script.commits.lock().unwrap().as_slice(), [true, true]);
}

#[test]
fn a_lost_answer_after_a_commit_fails_indeterminate() {
    let rig = rig(vec![SteerReply {
        commit: true,
        hold_before: None,
        hold: None,
        queued: false,
        answer: SteerAnswer::Rejected(ErrorCode::DuplicateCommand, "again".into()),
    }]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let output = rig.tool.run(
        &args(&id.0, "hi"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(code_of(&output), ErrorCode::Indeterminate);
    assert_eq!(
        content(&output),
        format!(
            "Delegate {} received the message, but its answer was lost; it joins the delegate's turn unless the delegate refused it.\n",
            id.0
        )
    );
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_committed_send_to_an_ended_run_fails_indeterminate_without_resuming() {
    let rig = rig(vec![SteerReply::gone()]);
    let (id, finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    assert!(
        rig.script.await_calls(1, MUST_SUCCEED_WITHIN),
        "the attempt committed"
    );
    assert!(
        rig.clock.await_parked(
            rig.clock.now() + Duration::from_millis(50),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the call parks its backoff"
    );
    // The run ends under the committed attempt, as another call's resume
    // would end it.
    end_run(finish, &id);
    let output = answered(&rx);
    handle.join().unwrap();
    assert_eq!(code_of(&output), ErrorCode::Indeterminate);
    assert_eq!(
        content(&output),
        format!(
            "Delegate {} ended before answering; the message may have joined its last turn. Send it again to resume the delegate.\n",
            id.0
        )
    );
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_run_that_changes_under_a_committed_attempt_is_not_sent_twice() {
    let rig = rig(vec![SteerReply::gone()]);
    let (id, finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    assert!(
        rig.script.await_calls(1, MUST_SUCCEED_WITHIN),
        "the attempt committed"
    );
    assert!(
        rig.clock.await_parked(
            rig.clock.now() + Duration::from_millis(50),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the call parks its backoff"
    );
    // Another call ends run 1 and resumes it to run 2.
    end_run(finish, &id);
    let _held = match rig.registry.resume(&id.0, None, |_| {
        Ok::<_, String>((contract::jobs::Stop(Box::new(|| {})), ()))
    }) {
        Ok(resumed) => resumed,
        Err(_) => panic!("the other resume committed"),
    };
    let output = answered(&rx);
    handle.join().unwrap();
    assert_eq!(code_of(&output), ErrorCode::Indeterminate);
    assert!(content(&output).contains("ended before answering"));
    assert_eq!(rig.script.calls().len(), 1, "no send to run 2");
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_stale_commit_is_withheld_and_never_resent() {
    let (hold_tx, hold_rx) = mpsc::channel();
    let rig = rig(vec![
        SteerReply::gone(),
        SteerReply {
            commit: true,
            hold_before: Some(hold_rx),
            hold: None,
            queued: false,
            answer: SteerAnswer::Accepted,
        },
    ]);
    let (id, finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    let backoff = rig.clock.now() + Duration::from_millis(50);
    assert!(
        rig.clock
            .await_parked(backoff, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its backoff"
    );
    rig.clock.advance_when_parked(
        Some(backoff),
        None,
        Duration::from_millis(50),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert!(
        rig.script.await_calls(2, MUST_SUCCEED_WITHIN),
        "the resend held"
    );
    // Run 1 ends and resumes to run 2 while the resend is held before
    // its commit.
    end_run(finish, &id);
    let _held = match rig.registry.resume(&id.0, None, |_| {
        Ok::<_, String>((contract::jobs::Stop(Box::new(|| {})), ()))
    }) {
        Ok(resumed) => resumed,
        Err(_) => panic!("the other resume committed"),
    };
    hold_tx.send(()).unwrap();
    let output = answered(&rx);
    handle.join().unwrap();
    assert_eq!(code_of(&output), ErrorCode::Indeterminate);
    assert!(content(&output).contains("ended before answering"));
    assert_eq!(
        rig.script.commits.lock().unwrap().as_slice(),
        [true, false],
        "the stale commit was refused"
    );
    assert_eq!(rig.script.calls().len(), 2, "no further attempt");
}

#[test]
fn a_withheld_commit_asks_again_and_steers() {
    let rig = rig(vec![
        SteerReply {
            commit: false,
            hold_before: None,
            hold: None,
            queued: false,
            answer: SteerAnswer::Accepted,
        },
        SteerReply::accept(),
    ]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let output = rig.tool.run(
        &args(&id.0, "hi"),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert!(content(&output).starts_with(&format!("Steered delegate {}", id.0)));
    assert_eq!(rig.script.calls().len(), 2);
    assert_eq!(rig.script.commits.lock().unwrap().as_slice(), [false, true]);
}

#[test]
fn a_withheld_answer_while_cancelled_stops_without_another_attempt() {
    let rig = rig(vec![]);
    let gate = Arc::new(Mutex::new(super::SteerGate {
        cancelled: false,
        sent_to: None,
    }));
    let cancel = CancelToken::new();
    cancel.cancel();
    // No thread and no race: a withheld answer with the call cancelled
    // stops, instead of asking `target` again.
    match rig.tool.answer(
        Steered::Withheld,
        "j_x",
        "hi",
        1,
        Duration::from_millis(50),
        &None,
        &gate,
        &cancel,
    ) {
        Attempt::Done(output) => {
            assert!(output.error.is_none());
            assert!(
                content(&output).contains("nothing was delivered"),
                "{}",
                content(&output)
            );
        }
        Attempt::Again(_) | Attempt::Found(..) => {
            panic!("a withheld answer while cancelled stops")
        }
    }
}

#[test]
fn closing_while_the_run_lives_fails_closing_at_the_bound() {
    let rig = rig(vec![SteerReply::rejected(ErrorCode::Closing, "done")]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        CancelToken::new(),
        Arc::new(Recorder::default()),
    );
    let at = rig.clock.now() + BOUND;
    assert!(
        rig.clock
            .await_parked(at, fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call parks its closing wait"
    );
    let mark = rig.clock.advance_when_parked(
        Some(at),
        None,
        BOUND
            .checked_sub(Duration::from_millis(1))
            .expect("the bound exceeds 1 ms"),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    // The advance woke the closing wait; it re-parks at the same bound
    // past its own clock check. Only then is "still parked" decided,
    // by a state check, never a sleep.
    assert!(
        rig.clock
            .await_parked_since(&mark, Some(at), fakes::Deadline::after(MUST_SUCCEED_WITHIN)),
        "the call re-parks its closing wait"
    );
    assert!(rx.try_recv().is_err(), "bound minus 1 ms still parked");
    rig.clock.advance_when_parked(
        Some(at),
        None,
        Duration::from_millis(1),
        &fakes::Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let output = answered(&rx);
    handle.join().unwrap();
    assert_eq!(code_of(&output), ErrorCode::Closing);
    assert_eq!(
        content(&output),
        format!(
            "Delegate {} is finishing its run and did not take the message; send it again once it has finished.\n",
            id.0
        )
    );
    assert_eq!(rig.launched.load(Ordering::SeqCst), 0);
}

#[test]
fn a_cancel_before_the_commit_delivers_nothing() {
    let (hold_tx, hold_rx) = mpsc::channel();
    let rig = rig(vec![SteerReply {
        commit: true,
        hold_before: Some(hold_rx),
        hold: None,
        queued: false,
        answer: SteerAnswer::Accepted,
    }]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let cancel = CancelToken::new();
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        cancel.clone(),
        Arc::new(Recorder::default()),
    );
    assert!(
        rig.script.await_holds(1, MUST_SUCCEED_WITHIN),
        "the steer held"
    );
    cancel.cancel();
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(
        content(&output),
        format!(
            "The call was cancelled before the message reached delegate {}; nothing was delivered.",
            id.0
        )
    );
    // Releasing afterwards, the commit is refused and nothing is written.
    hold_tx.send(()).unwrap();
    assert!(
        rig.script.await_commits(1, MUST_SUCCEED_WITHIN),
        "the steer recorded its refusal"
    );
    assert_eq!(
        *rig.script.commits.lock().unwrap(),
        vec![false],
        "the commit was offered and refused and nothing was written"
    );
}

#[test]
fn a_cancel_after_the_commit_says_the_message_may_still_join_its_turn() {
    let (hold_tx, hold_rx) = mpsc::channel();
    let rig = rig(vec![SteerReply {
        commit: true,
        hold_before: None,
        hold: Some(hold_rx),
        queued: false,
        answer: SteerAnswer::Accepted,
    }]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let emit = Arc::new(Recorder::default());
    let cancel = CancelToken::new();
    let (rx, handle) = call_async(&rig.tool, &id.0, "hi", cancel.clone(), Arc::clone(&emit));
    assert!(
        rig.script.await_holds(1, MUST_SUCCEED_WITHIN),
        "the write held"
    );
    assert_eq!(
        rig.script.commits.lock().unwrap().as_slice(),
        [true],
        "the send committed"
    );
    cancel.cancel();
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    assert_eq!(
        content(&output),
        format!(
            "The call was cancelled after sending to delegate {} was committed; the message may still join its turn.",
            id.0
        )
    );
    let deltas = emit.events().len();
    // Releasing afterwards, the queued mark emits no delta: nothing from
    // the call follows its return.
    hold_tx.send(()).unwrap();
    assert_eq!(emit.events().len(), deltas, "no late delta");
}

#[test]
fn a_cancel_during_the_backoff_delivers_nothing() {
    let rig = rig(vec![SteerReply::gone()]);
    let (id, _finish) = open_delegate(&rig, "scan");
    let cancel = CancelToken::new();
    let (rx, handle) = call_async(
        &rig.tool,
        &id.0,
        "hi",
        cancel.clone(),
        Arc::new(Recorder::default()),
    );
    assert!(
        rig.clock.await_parked(
            rig.clock.now() + Duration::from_millis(50),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the call parks its backoff"
    );
    cancel.cancel();
    let output = answered(&rx);
    handle.join().unwrap();
    assert!(output.error.is_none());
    // The backoff follows a committed send, which may still join its turn.
    assert!(content(&output).contains("may still join its turn"));
}
