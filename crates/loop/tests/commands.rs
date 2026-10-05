//! Driver commands the loop answers at its drain (`docs/invocation.md`,
//! "Driver commands" and "Lifecycle").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{Decision, TurnOutcome};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::provider::ToolDefinition;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, DeclaredEffects, Effect};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, RequestId};
use fakes::Scripted;
use serde_json::{Map, Value, json};

use support::{Session, TestTool, calls_reply, kinds, message};

const BUSY: &str = "A turn is running; send `steer` to add to it.";
const CLOSING: &str = "The session is closing and takes no new turn.";
const STALE_STEER: &str = "That steering message was already applied, or was never queued.";
const STALE_REPLY: &str = "That request is no longer pending.";
const UNFIT: &str = "That answer does not fit the pending request.";

fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (Ack(Box::new(move |answer| tx.send(answer).unwrap())), rx)
}

fn take(rx: &mpsc::Receiver<Answer>) -> Answer {
    rx.try_recv().expect("the command was answered")
}

fn accepted() -> Answer {
    Ok(None)
}

fn rejected(code: ErrorCode, message: &str) -> Answer {
    Err(Rejection {
        code,
        message: message.to_owned(),
    })
}

/// A prompt whose acknowledgement records whether `turn_started` was already
/// in the log.
fn watched_prompt(text: &str, dir: &Path) -> (Delivery, mpsc::Receiver<(Answer, bool)>) {
    let dir = dir.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let saw = log::read(&dir)
            .unwrap()
            .iter()
            .any(|line| line.kind == "turn_started");
        tx.send((answer, saw)).unwrap();
    }));
    (Delivery::Prompt(message(text), ack), rx)
}

fn steer_with(text: &str, ack: Ack) -> Delivery {
    Delivery::Steer(message(text), ack)
}

fn drop_of(text: &str, ack: Ack) -> Delivery {
    Delivery::SteerDrop(CommandId(format!("c_{text}")), ack)
}

fn reply_to(request_id: RequestId, answer: ReplyAnswer, ack: Ack) -> Delivery {
    Delivery::Reply(Reply { request_id, answer }, ack)
}

fn allow() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: None,
        remember: None,
    }
}

fn allow_with_feedback() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: Decision::Allow,
        feedback: Some("no".into()),
        remember: None,
    }
}

fn plain() -> Vec<&'static str> {
    vec![
        "session_started",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// `tool_turn` with `steering_applied` at the start of its last step: the
/// drain lists the queued steer, the step applies it, and the queue lists
/// empty once it is applied.
fn applied(middle: &[&str]) -> Vec<String> {
    let mut kinds = tool_turn(middle);
    let step = kinds
        .iter()
        .rposition(|kind| kind == "step_started")
        .unwrap();
    for (at, kind) in ["steering_queue", "steering_applied", "steering_queue"]
        .into_iter()
        .enumerate()
    {
        kinds.insert(step + 1 + at, kind.to_owned());
    }
    kinds
}

/// A turn whose first reply calls `once` and whose second says "Done.", with
/// `middle` between the reply and the next step.
fn tool_turn(middle: &[&str]) -> Vec<String> {
    let mut out = vec![
        "session_started",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "tool_call_arguments_delta",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
    ];
    out.extend(middle);
    out.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);
    out.into_iter().map(str::to_owned).collect()
}

/// One call's send, run once the tool is joined and before the next drain.
type Job = Box<dyn FnOnce(mpsc::Sender<Delivery>) + Send>;

/// One call sends the next queued job. The inbox sender is installed after
/// the session exists, and before the turn runs.
struct SendTool {
    inbox: Arc<Mutex<Option<mpsc::Sender<Delivery>>>>,
    jobs: Mutex<VecDeque<Job>>,
}

impl SendTool {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inbox: Arc::new(Mutex::new(None)),
            jobs: Mutex::new(VecDeque::new()),
        })
    }

    fn install(&self, inbox: mpsc::Sender<Delivery>) {
        *self.inbox.lock().unwrap() = Some(inbox);
    }

    fn push(&self, job: impl FnOnce(mpsc::Sender<Delivery>) + Send + 'static) {
        self.jobs.lock().unwrap().push_back(Box::new(job));
    }
}

impl Tool for SendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "once".into(),
            description: "Sends what the test queued.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
        })
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn contract::emit::Emit) -> Output {
        let inbox = self
            .inbox
            .lock()
            .unwrap()
            .clone()
            .expect("the inbox sender is installed");
        if let Some(job) = self.jobs.lock().unwrap().pop_front() {
            job(inbox);
        }
        Output {
            content: vec![ContentPart::Text { text: "ok".into() }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

fn once_session(script: Vec<Scripted>) -> (Arc<SendTool>, Session) {
    let tool = SendTool::new();
    let session = Session::with_tools(script, None, vec![Arc::clone(&tool) as Arc<dyn Tool>]);
    tool.install(session.inbox.clone());
    (tool, session)
}

fn ask_session(tool: Arc<TestTool>) -> Session {
    let session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    session.rules.set(standing_ask());
    session
}

/// An approval that was answered, then the call ran.
fn asked(after: &[&str]) -> Vec<String> {
    let mut middle = vec!["permission_requested", "permission_resolved"];
    middle.extend(after);
    tool_turn(&middle)
}

/// Two asks in one reply, both denied without a second request, then "Done.".
fn denied_pair() -> Vec<String> {
    [
        "session_started",
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
        "permission_requested",
        "permission_resolved",
        "permission_resolved",
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
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// A text reply continued by a second step: a steer taken at the end of
/// the turn lists the queue, and its drop on the next step lists it empty
/// again, with nothing applied.
fn continued() -> Vec<&'static str> {
    vec![
        "session_started",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "steering_queue",
        "step_started",
        "steering_queue",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// `close` during the final reply, then a steer into the turn still in
/// flight: the steer is applied on the step that continues it.
fn closed_and_steered() -> Vec<&'static str> {
    vec![
        "session_started",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "steering_queue",
        "step_started",
        "steering_applied",
        "steering_queue",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]
}

/// A steer applied on the second step, then a later drop that is too late.
fn applied_then_dropped() -> Vec<String> {
    [
        "session_started",
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
        "steering_queue",
        "steering_applied",
        "steering_queue",
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
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn shell(subject: &str) -> Arc<TestTool> {
    let mut tool = TestTool::declaring("shell", "Ran it.", vec![Effect::Executes], None);
    tool.subject = Some(subject.to_owned());
    Arc::new(tool)
}

fn standing_ask() -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision: RuleDecision::Ask,
            tool: "shell".into(),
            prefix: "npm publish".into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

fn paris() -> Value {
    json!({"city": "Paris"})
}

fn on_request(
    session: &Session,
    send: impl FnOnce(RequestId) + Send + 'static,
) -> thread::JoinHandle<()> {
    let mut watcher = session.log.watch();
    thread::spawn(move || {
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
                .recv_timeout(support::DEADLINE)
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

/// `run` until it returns. The session keeps its inbox sender, so the loop
/// ends because `close` was taken.
fn run_until_close(session: &mut Session) -> Vec<Envelope> {
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    let lines = session.lines();
    finished
        .recv_timeout(support::DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    lines
}

#[test]
fn a_prompt_is_accepted_once_its_turn_has_started() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (prompt, answers) = watched_prompt("hi", &session.dir);
    session.inbox.send(prompt).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let (answer, saw) = answers.try_recv().unwrap();
    assert_eq!(answer, accepted());
    assert!(saw, "turn_started is written before the prompt is accepted");
    assert_eq!(kinds(&session.lines()), plain());
}

#[test]
fn a_prompt_mid_turn_is_busy_and_starts_nothing() {
    let (tool, mut session) = once_session(vec![
        calls_reply("", &[("once", json!({}))]),
        Scripted::text("Done."),
    ]);
    let (ack, answers) = capture();
    tool.push(move |inbox| {
        inbox
            .send(Delivery::Prompt(message("another"), ack))
            .unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&answers), rejected(ErrorCode::Busy, BUSY));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        tool_turn(&["tool_call_started", "tool_call_completed"])
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "turn_started")
            .count(),
        1
    );
}

#[test]
fn two_prompts_waiting_while_idle_start_one_turn_and_the_second_is_busy() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (first, first_answers) = watched_prompt("one", &session.dir);
    let (second_ack, second_answers) = capture();
    session.inbox.send(first).unwrap();
    session
        .inbox
        .send(Delivery::Prompt(message("two"), second_ack))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let (answer, saw) = first_answers.try_recv().unwrap();
    assert_eq!(answer, accepted());
    assert!(saw);
    assert_eq!(take(&second_answers), rejected(ErrorCode::Busy, BUSY));
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(lines[1].payload["input"].as_array().unwrap().len(), 1);
}

#[test]
fn a_steer_mid_turn_is_accepted_and_applied_at_the_next_step() {
    let (tool, mut session) = once_session(vec![
        calls_reply("", &[("once", json!({}))]),
        Scripted::text("Done."),
    ]);
    let dir = session.dir.clone();
    let (tx, answers) = mpsc::channel();
    let ack = Ack(Box::new(move |answer| {
        let applied = log::read(&dir)
            .unwrap()
            .iter()
            .any(|line| line.kind == "steering_applied");
        tx.send((answer, applied)).unwrap();
    }));
    tool.push(move |inbox| inbox.send(steer_with("more", ack)).unwrap());
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let (answer, already) = answers.try_recv().unwrap();
    assert_eq!(answer, accepted());
    assert!(
        !already,
        "a steer is accepted when it is taken, before steering_applied"
    );
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        applied(&["tool_call_started", "tool_call_completed"])
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.kind == "steering_applied")
            .unwrap()
            .payload["content"][0]["text"],
        "more"
    );
}

#[test]
fn a_steer_while_idle_starts_a_turn() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session.inbox.send(steer_with("hello", ack)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&answers), accepted());
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(lines[1].payload["input"][0]["content"][0]["text"], "hello");
    assert!(
        lines.iter().all(|line| line.kind != "steering_applied"),
        "an idle steer is the turn's input"
    );
}

#[test]
fn a_steer_dropped_while_idle_is_not_the_turns_input() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (steer_ack, steer_answers) = capture();
    let (drop_ack, drop_answers) = capture();
    let (prompt, prompt_answers) = watched_prompt("go", &session.dir);
    session.inbox.send(steer_with("more", steer_ack)).unwrap();
    session.inbox.send(drop_of("more", drop_ack)).unwrap();
    session.inbox.send(prompt).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(take(&drop_answers), accepted());
    let (answer, saw) = prompt_answers.try_recv().unwrap();
    assert_eq!(answer, accepted());
    assert!(saw);
    let lines = session.lines();
    assert_eq!(kinds(&lines), plain());
    assert_eq!(lines[1].payload["input"][0]["content"][0]["text"], "go");
}

#[test]
fn a_drop_while_idle_removes_only_its_steer_after_the_prompt() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (prompt, _prompt_answers) = watched_prompt("go", &session.dir);
    let (one_ack, _one_answers) = capture();
    let (two_ack, _two_answers) = capture();
    let (drop_ack, drop_answers) = capture();
    session.inbox.send(prompt).unwrap();
    session.inbox.send(steer_with("one", one_ack)).unwrap();
    session.inbox.send(steer_with("two", two_ack)).unwrap();
    session.inbox.send(drop_of("two", drop_ack)).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&drop_answers), accepted());
    let lines = session.lines();
    let input = &lines[1].payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 2);
    assert_eq!(input[0]["content"][0]["text"], "go");
    assert_eq!(input[1]["content"][0]["text"], "one");
}

#[test]
fn a_steer_then_its_drop_in_one_drain_apply_nothing_and_both_are_accepted() {
    let (tool, mut session) = once_session(vec![
        calls_reply("", &[("once", json!({}))]),
        Scripted::text("Done."),
    ]);
    let (steer_ack, steer_answers) = capture();
    let (drop_ack, drop_answers) = capture();
    tool.push(move |inbox| {
        inbox.send(steer_with("more", steer_ack)).unwrap();
        inbox.send(drop_of("more", drop_ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(take(&drop_answers), accepted());
    // The steer lists the queue when taken, and the drop lists it again
    // when it removes the steer, leaving nothing to apply.
    let mut expected = tool_turn(&["tool_call_started", "tool_call_completed"]);
    let at = expected
        .iter()
        .rposition(|kind| kind == "step_started")
        .unwrap();
    expected.insert(at + 1, "steering_queue".to_owned());
    expected.insert(at + 2, "steering_queue".to_owned());
    assert_eq!(kinds(&session.lines()), expected);
}

#[test]
fn a_drop_before_its_steer_is_stale_and_the_steer_is_applied() {
    let (tool, mut session) = once_session(vec![
        calls_reply("", &[("once", json!({}))]),
        Scripted::text("Done."),
    ]);
    let (drop_ack, drop_answers) = capture();
    let (steer_ack, steer_answers) = capture();
    tool.push(move |inbox| {
        inbox.send(drop_of("more", drop_ack)).unwrap();
        inbox.send(steer_with("more", steer_ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        take(&drop_answers),
        rejected(ErrorCode::StaleRequest, STALE_STEER)
    );
    assert_eq!(take(&steer_answers), accepted());
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        applied(&["tool_call_started", "tool_call_completed"])
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.kind == "steering_applied")
            .unwrap()
            .payload["content"][0]["text"],
        "more"
    );
}

#[test]
fn a_drop_after_steering_applied_is_stale() {
    let (tool, mut session) = once_session(vec![
        calls_reply("", &[("once", json!({}))]),
        calls_reply("", &[("once", json!({}))]),
        Scripted::text("Done."),
    ]);
    let (steer_ack, steer_answers) = capture();
    let (drop_ack, drop_answers) = capture();
    // The first call sends the steer, which the next step applies. The
    // second call sends the drop, after that `steering_applied`.
    tool.push(move |inbox| inbox.send(steer_with("more", steer_ack)).unwrap());
    tool.push(move |inbox| inbox.send(drop_of("more", drop_ack)).unwrap());
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(
        take(&drop_answers),
        rejected(ErrorCode::StaleRequest, STALE_STEER)
    );
    assert_eq!(kinds(&session.lines()), applied_then_dropped());
}

#[test]
fn a_steer_held_during_an_approval_can_be_dropped() {
    let tool = shell("npm publish");
    let mut session = ask_session(Arc::clone(&tool));
    let inbox = session.inbox.clone();
    let (steer_ack, steer_answers) = capture();
    let (drop_ack, drop_answers) = capture();
    let (reply_ack, reply_answers) = capture();
    let answered = on_request(&session, move |id| {
        inbox.send(steer_with("wait", steer_ack)).unwrap();
        inbox.send(drop_of("wait", drop_ack)).unwrap();
        inbox.send(reply_to(id, allow(), reply_ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(take(&drop_answers), accepted());
    assert_eq!(take(&reply_answers), accepted());
    let lines = session.lines();
    // The steer queued during the approval lists the queue, and its drop
    // lists it again, before the person's answer resolves the request.
    let mut expected = asked(&["tool_call_started", "tool_call_completed"]);
    let at = expected
        .iter()
        .position(|kind| kind == "permission_requested")
        .unwrap();
    expected.insert(at + 1, "steering_queue".to_owned());
    expected.insert(at + 2, "steering_queue".to_owned());
    assert_eq!(kinds(&lines), expected);
    assert!(lines.iter().all(|line| line.kind != "steering_applied"));
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_drop_of_a_steer_taken_at_the_end_of_a_turn_applies_nothing() {
    let slot: Arc<Mutex<Option<mpsc::Sender<Delivery>>>> = Arc::new(Mutex::new(None));
    let (drop_ack, drop_answers) = capture();
    let (steer_tx, steer_answers) = mpsc::channel();
    let slot_for_ack = Arc::clone(&slot);
    let steer_ack = Ack(Box::new(move |answer| {
        let inbox = slot_for_ack.lock().unwrap().clone().unwrap();
        inbox.send(drop_of("later", drop_ack)).unwrap();
        steer_tx.send(answer).unwrap();
    }));
    let mut session = Session::injecting(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        vec![steer_with("later", steer_ack)],
    );
    *slot.lock().unwrap() = Some(session.inbox.clone());
    session.inbox.send(support::delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(take(&drop_answers), accepted());
    assert_eq!(kinds(&session.lines()), continued());
}

#[test]
fn a_reply_to_the_pending_request_is_accepted_and_resolves_it() {
    let tool = shell("npm publish");
    let mut session = ask_session(Arc::clone(&tool));
    let inbox = session.inbox.clone();
    let (stale_ack, stale_answers) = capture();
    let (ack, answers) = capture();
    let answered = on_request(&session, move |id| {
        inbox
            .send(reply_to(RequestId("r_other".into()), allow(), stale_ack))
            .unwrap();
        inbox.send(reply_to(id, allow(), ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(
        take(&stale_answers),
        rejected(ErrorCode::StaleRequest, STALE_REPLY)
    );
    assert_eq!(take(&answers), accepted());
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        asked(&["tool_call_started", "tool_call_completed"])
    );
    let resolved = lines
        .iter()
        .find(|line| line.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["decision"], "allow");
    assert_eq!(resolved.payload["decided_by"], "person");
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_reply_that_does_not_fit_is_rejected_and_a_fitting_one_still_resolves() {
    let tool = shell("npm publish");
    let mut session = ask_session(Arc::clone(&tool));
    let inbox = session.inbox.clone();
    let (bad_ack, bad_answers) = capture();
    let (ack, answers) = capture();
    let answered = on_request(&session, move |id| {
        inbox
            .send(reply_to(id.clone(), allow_with_feedback(), bad_ack))
            .unwrap();
        inbox.send(reply_to(id, allow(), ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(
        take(&bad_answers),
        rejected(ErrorCode::InvalidArguments, UNFIT)
    );
    assert_eq!(take(&answers), accepted());
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        asked(&["tool_call_started", "tool_call_completed"])
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.kind == "permission_resolved")
            .unwrap()
            .payload["decision"],
        "allow"
    );
    assert_eq!(tool.ran().len(), 1);
}

#[test]
fn a_reply_with_nothing_pending_is_stale_and_writes_no_line() {
    let (idle_ack, idle_answers) = capture();
    let (later_ack, later_answers) = capture();
    let mut session = Session::injecting(
        vec![Scripted::text("Hi.")],
        vec![Delivery::Reply(
            Reply {
                request_id: RequestId("r_none".into()),
                answer: allow(),
            },
            later_ack,
        )],
    );
    session
        .inbox
        .send(reply_to(RequestId("r_idle".into()), allow(), idle_ack))
        .unwrap();
    session.inbox.send(support::delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        take(&idle_answers),
        rejected(ErrorCode::StaleRequest, STALE_REPLY)
    );
    assert_eq!(
        take(&later_answers),
        rejected(ErrorCode::StaleRequest, STALE_REPLY)
    );
    assert_eq!(kinds(&session.lines()), plain());
}

#[test]
fn a_drop_of_an_unknown_steer_is_stale() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    session.inbox.send(drop_of("missing", ack)).unwrap();
    session.inbox.send(support::delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(
        take(&answers),
        rejected(ErrorCode::StaleRequest, STALE_STEER)
    );
    assert_eq!(kinds(&session.lines()), plain());
}

#[test]
fn close_while_idle_ends_run() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let (ack, answers) = capture();
    let (steer_ack, steer_answers) = capture();
    session.inbox.send(Delivery::Close(ack)).unwrap();
    session.inbox.send(steer_with("later", steer_ack)).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    finished
        .recv_timeout(support::DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    assert_eq!(take(&answers), accepted());
    assert_eq!(take(&steer_answers), rejected(ErrorCode::Closing, CLOSING));
    assert_eq!(
        log::read(&session.dir)
            .unwrap()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["session_started"]
    );
    assert!(session.provider.requests().is_empty());
}

#[test]
fn close_mid_turn_lets_the_turn_finish_and_a_later_prompt_is_closing() {
    let (close_ack, close_answers) = capture();
    let (prompt_ack, prompt_answers) = capture();
    let (steer_ack, steer_answers) = capture();
    let mut session = Session::injecting(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        vec![
            Delivery::Close(close_ack),
            Delivery::Prompt(message("next"), prompt_ack),
            steer_with("more", steer_ack),
        ],
    );
    session.inbox.send(support::delivery("hi")).unwrap();
    let lines = run_until_close(&mut session);
    assert_eq!(take(&close_answers), accepted());
    assert_eq!(take(&prompt_answers), rejected(ErrorCode::Closing, CLOSING));
    assert_eq!(take(&steer_answers), accepted());
    assert_eq!(kinds(&lines), closed_and_steered());
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == "turn_started")
            .count(),
        1
    );
}

#[test]
fn close_during_an_approval_resolves_it_as_unanswerable() {
    let tool = shell("npm publish");
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris()), ("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
    );
    session.rules.set(standing_ask());
    let inbox = session.inbox.clone();
    let (close_ack, close_answers) = capture();
    let (reply_ack, reply_answers) = capture();
    let answered = on_request(&session, move |id| {
        inbox.send(Delivery::Close(close_ack)).unwrap();
        inbox.send(reply_to(id, allow(), reply_ack)).unwrap();
    });
    session.inbox.send(support::delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    answered.join().unwrap();
    assert_eq!(take(&close_answers), accepted());
    assert_eq!(
        take(&reply_answers),
        rejected(ErrorCode::StaleRequest, STALE_REPLY)
    );
    let lines = session.lines();
    assert_eq!(kinds(&lines), denied_pair());
    let resolved: Vec<_> = lines
        .iter()
        .filter(|line| line.kind == "permission_resolved")
        .collect();
    assert_eq!(resolved[0].payload["decision"], "deny");
    assert_eq!(resolved[0].payload["decided_by"], "standing_rule");
    assert_eq!(
        resolved[0].payload["reason"],
        "No person can answer an approval in this session."
    );
    assert!(resolved[0].payload.get("request_id").is_some());
    assert_eq!(resolved[1].payload["decision"], "deny");
    assert_eq!(resolved[1].payload["decided_by"], "standing_rule");
    assert!(resolved[1].payload.get("request_id").is_none());
    assert!(tool.ran().is_empty());
    for done in lines
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
    {
        assert_eq!(done.payload["status"], "denied");
        assert_eq!(done.payload["reason"], "no_person");
    }
}
