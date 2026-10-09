//! A question that suspends lives like a pending approval
//! (`docs/tools.md`, "When a person can answer"): once its call is the
//! step's only call without a result, the idle delay or a shutdown ends
//! the step with it still pending, and `fiber_exited` names it
//! (`docs/invocation.md`, "Lifecycle" and "Shutdown"). While another call
//! of the step has no result, the step waits for the answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock as _;
use contract::commands::{Reply, ReplyAnswer};
use contract::emit::Emit;
use contract::events::{Answer, Control, Event, Interaction, TurnOutcome};
use contract::inbox::{Ack, Delivery};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::provider::Input;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Question};
use contract::tool::{Answered, Ask, Asking, Cancel, Effects, EffectsError, Output, Tool};
use contract::{ActionId, Envelope, ErrorCode, JobId, RequestId};
use fakes::Scripted;
use fakes::clock::FakeClock;
use r#loop::{Error, Loop};
use serde_json::{Map, Value, json};

use support::{DEADLINE, Gate, Session, Tap, TestTool, calls_reply, delivery, kinds};

/// The idle delay every test but the no-timeout one sets.
const IDLE: Duration = Duration::from_secs(60);

/// The text a call returns when its questions go to the driver.
const SENT: &str = "The questions went to the driver.";

/// A tool that asks as `ask_user` does: one `form` of its questions that
/// suspends, or the questions for the driver when nobody can answer. It
/// records the arguments of each run.
struct Former {
    suspends: bool,
    runs: Mutex<Vec<Map<String, Value>>>,
}

impl Former {
    fn new() -> Self {
        Self {
            suspends: true,
            runs: Mutex::default(),
        }
    }

    /// One whose form does not suspend.
    fn plain() -> Self {
        Self {
            suspends: false,
            ..Self::new()
        }
    }

    fn runs(&self) -> Vec<Map<String, Value>> {
        self.runs.lock().unwrap().clone()
    }
}

fn text(text: &str) -> Output {
    Output {
        content: vec![ContentPart::Text { text: text.into() }],
        ..Output::default()
    }
}

fn to_driver(questions: Vec<Question>) -> Output {
    Output {
        control: Some(Control {
            handoff: None,
            questions: Some(questions),
            skill: None,
        }),
        ..text(SENT)
    }
}

impl Tool for Former {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "former".into(),
            description: "Asks the person a form.".into(),
            input_schema: json!({"type": "object"}),
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
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
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
            suspends: self.suspends,
        });
        match answered {
            Answered::Reply(Answer::Declined { .. }) => text("declined"),
            Answered::Reply(answer) => text(&serde_json::to_string(&answer).unwrap()),
            Answered::NoAnswer if cancel.is_cancelled() => text("declined"),
            Answered::NoAnswer => to_driver(questions),
        }
    }
}

/// A tool whose call returns only once its gate opens.
struct Blocker {
    name: &'static str,
    gate: Arc<Gate>,
}

impl Tool for Blocker {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.into(),
            description: "Waits.".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Former::new().effects(&Map::new())
    }

    fn run(&self, _: &Map<String, Value>, _: &dyn Cancel, _: &dyn Emit) -> Output {
        self.gate.wait();
        text("waited")
    }
}

fn blocker(name: &'static str) -> (Arc<Blocker>, Arc<Gate>) {
    let gate = Arc::new(Gate::default());
    let tool = Blocker {
        name,
        gate: Arc::clone(&gate),
    };
    (Arc::new(tool), gate)
}

/// Jobs a test turns on and off: one job runs while `running` is set.
#[derive(Default)]
struct Toggle {
    running: AtomicBool,
}

impl Jobs for Toggle {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: PathBuf::from("jobs"),
            source: std::io::Error::other("no jobs here"),
        })
    }

    fn stop(&self, _job_id: &JobId) -> bool {
        false
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        if self.running.load(Ordering::SeqCst) {
            vec![JobId("j_1".into())]
        } else {
            Vec::new()
        }
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

/// The arguments of a `former` call: one question with two options.
fn questions() -> Value {
    json!({"questions": [{
        "header": "Base",
        "question": "Which branch?",
        "options": [{"label": "main"}, {"label": "dev"}],
    }]})
}

/// A session whose first reply makes `calls` and whose second says
/// "Done.", with `tools` registered, an inbox wake, and `idle` as the idle
/// delay.
fn session(calls: &[&str], tools: Vec<Arc<dyn Tool>>, idle: Option<Duration>) -> Session {
    let calls: Vec<(&str, Value)> = calls
        .iter()
        .map(|name| {
            // `unread`'s call does not fit its schema, so it fails
            // without running.
            let arguments = match *name {
                "former" => questions(),
                "unread" => json!({}),
                _ => json!({"city": "Paris"}),
            };
            (*name, arguments)
        })
        .collect();
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        tools,
    )
    .inbox_woken();
    session.looped = session.looped.take().map(|looped| looped.idle_exit(idle));
    session
}

type Finished = mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, Error>)>;

/// Sends a prompt and runs one turn on its own thread.
fn start(session: &mut Session) -> Finished {
    session.inbox.send(delivery("go")).unwrap();
    run_turn(session)
}

/// Runs one turn on its own thread.
fn run_turn(session: &mut Session) -> Finished {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    finished
}

/// Waits for the turn `start` started, putting the loop back.
fn finish(session: &mut Session, finished: &Finished) -> Option<TurnOutcome> {
    let (looped, outcome) = finished
        .recv_timeout(DEADLINE)
        .expect("the turn ended in time");
    session.looped = Some(looped);
    outcome.unwrap()
}

/// Asserts the turn has not ended.
fn still_running(finished: &Finished) {
    assert!(finished.try_recv().is_err(), "the turn has not ended");
}

/// An ack that reports how its command was answered.
fn acked() -> (Ack, mpsc::Receiver<contract::inbox::Answer>) {
    let (tx, rx) = mpsc::channel();
    let ack = Ack(Box::new(move |answered| {
        let _sent = tx.send(answered);
    }));
    (ack, rx)
}

/// Sends `answer` to `request`, and returns where its answer arrives.
fn send_reply(
    session: &Session,
    request: &str,
    answer: Value,
) -> mpsc::Receiver<contract::inbox::Answer> {
    let (ack, rx) = acked();
    let reply = Reply {
        request_id: RequestId(request.into()),
        answer: serde_json::from_value::<ReplyAnswer>(answer).unwrap(),
    };
    session.inbox.send(Delivery::Reply(reply, ack)).unwrap();
    rx
}

/// Sends `answer` to `request` and returns how the command was answered.
fn answer(session: &Session, request: &str, answer: Value) -> contract::inbox::Answer {
    let rx = send_reply(session, request, answer);
    rx.recv_timeout(DEADLINE).expect("the reply is answered")
}

/// A reply that fits `questions()`.
fn main_branch() -> Value {
    json!({"answers": [{"labels": ["main"]}]})
}

fn request_id(line: &Envelope) -> String {
    line.payload["request_id"].as_str().unwrap().to_owned()
}

fn of_kind<'a>(lines: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    lines.iter().filter(|line| line.kind == kind).collect()
}

fn no_resolution(tap: &Tap) {
    assert!(
        tap.pending()
            .iter()
            .all(|line| line.kind != "interaction_resolved"),
        "the form is still pending"
    );
}

/// Writes `fiber_exited` as the process does at exit, and returns every
/// line of the session through it.
fn exit(session: &mut Session, signal: Option<i32>) -> Vec<Envelope> {
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, signal).unwrap();
    session.events_until("fiber_exited", |line| line.kind == "fiber_exited")
}

/// One call in the first reply.
const OPENING: &[&str] = &[
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

/// Two calls in the first reply.
const OPENING_TWO: &[&str] = &[
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
];

/// The second step, which says "Done.".
const DONE: &[&str] = &[
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

fn assert_kinds(lines: &[Envelope], parts: &[&[&str]]) {
    assert_eq!(kinds(lines), parts.concat());
}

/// Waits until the step parks at `at`, then advances the clock to 1 ms
/// before it and checks the step waits again with the form pending, then
/// to `at`.
fn idle_passes(session: &Session, tap: &Tap, finished: &Finished, at: Instant) {
    let clock = Arc::clone(&session.clock);
    assert!(
        clock.await_parked(at, DEADLINE),
        "the step waits until the idle deadline"
    );
    let left = at
        .duration_since(clock.now())
        .saturating_sub(Duration::from_millis(1));
    let mark = clock.advance_marked(left);
    assert!(
        clock.await_parked_since(&mark, Some(at), DEADLINE),
        "1 ms before it, the step waits again"
    );
    no_resolution(tap);
    still_running(finished);
    clock.advance(Duration::from_millis(1));
}

/// Whether the loop thread settles into a wait on the clock, rather than
/// spinning: a park caught by a zero advance is followed by another park.
fn settles(clock: &FakeClock) -> bool {
    clock.await_parked_unbounded(DEADLINE) && {
        let mark = clock.advance_marked(Duration::ZERO);
        clock.await_parked_since(&mark, None, DEADLINE)
    }
}

#[test]
fn the_idle_delay_exits_on_a_form_that_is_the_steps_only_call() {
    let former = Arc::new(Former::new());
    let mut session = session(&["former"], vec![former.clone()], Some(IDLE));
    let tap = Tap::new(&session.log);
    let at = session.clock.now() + IDLE;
    let finished = start(&mut session);
    let requested = tap.wait_for("interaction_requested");
    assert_eq!(requested.payload["resumes"], true);
    idle_passes(&session, &tap, &finished, at);

    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, None);
    assert_kinds(
        &lines,
        &[
            OPENING,
            &["tool_call_started", "interaction_requested", "fiber_exited"],
        ],
    );
    let exited = of_kind(&lines, "fiber_exited")[0];
    assert_eq!(exited.payload["suspended_on"], request_id(&requested));
    assert_eq!(former.runs().len(), 1);
}

#[test]
fn a_rejected_reply_does_not_move_the_idle_deadline() {
    let mut session = session(&["former"], vec![Arc::new(Former::new())], Some(IDLE));
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let at = clock.now() + IDLE;
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    assert!(clock.await_parked(at, DEADLINE), "the step waits until it");
    clock.advance(IDLE / 2);
    let mark = clock
        .mark_parked(at, DEADLINE)
        .expect("the step waits again");
    let answered = answer(&session, &request, json!({"confirmed": true}));
    assert_eq!(answered.unwrap_err().code, ErrorCode::InvalidArguments);
    assert!(
        clock.await_parked_since(&mark, Some(at), DEADLINE),
        "after the rejected reply the step waits until the same deadline"
    );
    idle_passes(&session, &tap, &finished, at);

    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, None);
    assert_kinds(
        &lines,
        &[
            OPENING,
            &["tool_call_started", "interaction_requested", "fiber_exited"],
        ],
    );
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request
    );
    assert!(of_kind(&lines, "interaction_resolved").is_empty());
}

/// Runs a step whose form comes before a call of `later`, and checks the
/// step still waits for the answer past the idle delay, then writes both
/// completions in request order.
fn waits_past_the_idle_delay(later: &'static str) {
    let other = Arc::new(TestTool::reads(later, "read"));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Former::new()), other.clone()];
    let mut session = session(&["former", later], tools, Some(IDLE));
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the step waits for the answer with no deadline"
    );
    let mark = clock.advance_marked(IDLE * 2);
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "past the idle delay the step still waits with no deadline"
    );
    // A stale reply is answered from the step's wait, after the advance.
    let stale = answer(&session, "r_nope", main_branch());
    assert_eq!(stale.unwrap_err().code, ErrorCode::StaleRequest);
    still_running(&finished);
    no_resolution(&tap);
    assert_eq!(answer(&session, &request, main_branch()), Ok(None));

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    // A call that fails its schema never starts: only `reads` writes the
    // second `tool_call_started`.
    let mut middle = vec!["tool_call_started"];
    if later == "reads" {
        middle.push("tool_call_started");
    }
    middle.extend([
        "interaction_requested",
        "interaction_resolved",
        "tool_call_completed",
        "tool_call_completed",
    ]);
    assert_kinds(&lines, &[OPENING_TWO, &middle, DONE]);
    let requested = of_kind(&lines, "tool_call_requested");
    let completed: Vec<_> = of_kind(&lines, "tool_call_completed")
        .iter()
        .map(|line| line.action_id.clone())
        .collect();
    let asked: Vec<_> = requested
        .iter()
        .map(|line| line.action_id.clone())
        .collect();
    assert_eq!(completed, asked, "completions in request order");
    assert_eq!(of_kind(&lines, "interaction_resolved").len(), 1);
    let ran = usize::from(later == "reads");
    assert_eq!(other.ran().len(), ran);
}

#[test]
fn a_form_before_a_call_that_ran_waits_for_its_answer_past_the_idle_delay() {
    waits_past_the_idle_delay("reads");
}

#[test]
fn a_form_before_a_call_that_failed_waits_for_its_answer_past_the_idle_delay() {
    waits_past_the_idle_delay("unread");
}

#[test]
fn the_idle_delay_counts_from_when_the_earlier_call_completes() {
    let (waits, gate) = blocker("waits");
    let tools: Vec<Arc<dyn Tool>> = vec![waits, Arc::new(Former::new())];
    let mut session = session(&["waits", "former"], tools, Some(IDLE));
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the step waits for the earlier call with no deadline"
    );
    let mark = clock.advance_marked(IDLE);
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "the advance leaves the step waiting with no deadline"
    );
    gate.open();
    tap.wait_for("tool_call_completed");
    let at = clock.now() + IDLE;
    idle_passes(&session, &tap, &finished, at);

    assert_eq!(finish(&mut session, &finished), None);
    gate.check("waits");
    let lines = exit(&mut session, None);
    assert_kinds(
        &lines,
        &[
            OPENING_TWO,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "tool_call_completed",
                "fiber_exited",
            ],
        ],
    );
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request
    );
}

#[test]
fn a_running_job_keeps_a_pending_form_from_going_idle() {
    let mut session = session(&["former"], vec![Arc::new(Former::new())], Some(IDLE));
    let jobs = Arc::new(Toggle::default());
    jobs.running.store(true, Ordering::SeqCst);
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.jobs(Arc::clone(&jobs) as Arc<dyn Jobs>));
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "with a job running the step waits with no deadline"
    );
    let mark = clock.advance_marked(IDLE * 2);
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "past the idle delay the step still waits with no deadline"
    );
    still_running(&finished);
    no_resolution(&tap);
    // The job ends: the delay counts from the wait that sees it.
    jobs.running.store(false, Ordering::SeqCst);
    session.inbox.send(Delivery::Cancelled).unwrap();
    let at = clock.now() + IDLE;
    idle_passes(&session, &tap, &finished, at);

    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, None);
    assert_kinds(
        &lines,
        &[
            OPENING,
            &["tool_call_started", "interaction_requested", "fiber_exited"],
        ],
    );
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request
    );
}

#[test]
fn a_shutdown_leaves_a_form_pending_when_it_is_the_last_call_without_a_result() {
    let (waits, gate) = blocker("waits");
    let tools: Vec<Arc<dyn Tool>> = vec![waits, Arc::new(Former::new())];
    let mut session = session(&["waits", "former"], tools, Some(IDLE));
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    session.cancel.shutdown(143);
    assert!(
        settles(&session.clock),
        "the step waits for the earlier call to stop"
    );
    gate.open();

    assert_eq!(finish(&mut session, &finished), None);
    gate.check("waits");
    let lines = exit(&mut session, Some(143));
    assert_kinds(
        &lines,
        &[
            OPENING_TWO,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "tool_call_completed",
                "fiber_exited",
            ],
        ],
    );
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["status"],
        "cancelled"
    );
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request
    );
}

#[test]
fn a_shutdown_resolves_a_form_with_a_later_call_without_a_result() {
    let (waits, gate) = blocker("waits");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Former::new()), waits];
    let mut session = session(&["former", "waits"], tools, Some(IDLE));
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    tap.wait_for("interaction_requested");
    session.cancel.shutdown(143);
    let resolved = tap.wait_for("interaction_resolved");
    assert_eq!(resolved.payload["by"], "fiber");
    assert_eq!(resolved.payload["declined"], true);
    gate.open();

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Interrupted)
    );
    gate.check("waits");
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING_TWO,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    let completed = of_kind(&lines, "tool_call_completed");
    assert_eq!(completed[0].payload["status"], "cancelled");
    assert_eq!(completed[1].payload["status"], "cancelled");
}

#[test]
fn a_cancel_resolves_a_form_that_is_the_last_call_without_a_result() {
    let (waits, gate) = blocker("waits");
    let tools: Vec<Arc<dyn Tool>> = vec![waits, Arc::new(Former::new())];
    let mut session = session(&["waits", "former"], tools, Some(IDLE));
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    tap.wait_for("interaction_requested");
    assert!(session.cancel.cancel());
    let resolved = tap.wait_for("interaction_resolved");
    assert_eq!(resolved.payload["by"], "fiber");
    assert_eq!(resolved.payload["declined"], true);
    gate.open();

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Interrupted)
    );
    gate.check("waits");
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING_TWO,
            &[
                "tool_call_started",
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    let completed = of_kind(&lines, "tool_call_completed");
    assert_eq!(completed[1].payload["status"], "cancelled");
    assert_eq!(
        completed[1].payload["content"],
        json!([{"type": "text", "text": "declined"}])
    );
}

#[test]
fn a_cancel_resolves_a_form_that_is_the_steps_only_call() {
    let mut session = session(&["former"], vec![Arc::new(Former::new())], Some(IDLE));
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    tap.wait_for("interaction_requested");
    assert!(session.cancel.cancel());

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Interrupted)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    assert_eq!(
        of_kind(&lines, "interaction_resolved")[0].payload["by"],
        "fiber"
    );
}

#[test]
fn without_an_idle_delay_a_form_waits_a_day_for_its_answer() {
    let mut session = session(&["former"], vec![Arc::new(Former::new())], None);
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let request = request_id(&tap.wait_for("interaction_requested"));
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the step waits with no deadline"
    );
    let mark = clock.advance_marked(Duration::from_secs(24 * 60 * 60));
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "a day later the step still waits with no deadline"
    );
    still_running(&finished);
    no_resolution(&tap);
    assert_eq!(answer(&session, &request, main_branch()), Ok(None));

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["content"],
        json!([{"type": "text", "text": "{\"answers\":[{\"labels\":[\"main\"]}]}"}])
    );
}

#[test]
fn an_ask_that_does_not_suspend_keeps_the_session_past_the_idle_delay() {
    let mut session = session(&["former"], vec![Arc::new(Former::plain())], Some(IDLE));
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    let finished = start(&mut session);
    let requested = tap.wait_for("interaction_requested");
    assert!(requested.payload.get("resumes").is_none());
    assert!(
        clock.await_parked_unbounded(DEADLINE),
        "the step waits for the answer with no deadline"
    );
    let mark = clock.advance_marked(IDLE * 2);
    assert!(
        clock.await_parked_since(&mark, None, DEADLINE),
        "past the idle delay the step still waits with no deadline"
    );
    let stale = answer(&session, "r_nope", main_branch());
    assert_eq!(stale.unwrap_err().code, ErrorCode::StaleRequest);
    still_running(&finished);
    no_resolution(&tap);
    let request = request_id(&requested);
    assert_eq!(answer(&session, &request, main_branch()), Ok(None));
    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            OPENING,
            &[
                "tool_call_started",
                "interaction_requested",
                "interaction_resolved",
                "tool_call_completed",
            ],
            DONE,
        ],
    );
}

/// A session shut down on its form, the form's `interaction_requested`,
/// and the lines through `fiber_exited`.
type ShutDown = (Session, Envelope, Vec<Envelope>);

/// Runs a session whose only call is `former`'s form until the step waits
/// on the pending form, then shuts it down. A question with no timeout
/// lives like a pending approval, so the shutdown alone ends the step
/// with the form still pending; the idle delay never fires. Returns the
/// session, the form's `interaction_requested`, and the lines through
/// `fiber_exited`.
fn shut_down_on_form(former: &Arc<Former>) -> ShutDown {
    let mut session = session(&["former"], vec![former.clone()], Some(IDLE));
    let tap = Tap::new(&session.log);
    let at = session.clock.now() + IDLE;
    let finished = start(&mut session);
    let requested = tap.wait_for("interaction_requested");
    assert!(
        session.clock.await_parked(at, DEADLINE),
        "the step waits on the pending form"
    );
    session.cancel.shutdown(143);
    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, Some(143));
    (session, requested, lines)
}

#[test]
fn a_shutdown_leaves_a_form_that_is_the_steps_only_call_pending() {
    let former = Arc::new(Former::new());
    let (_session, requested, lines) = shut_down_on_form(&former);
    assert_kinds(
        &lines,
        &[
            OPENING,
            &["tool_call_started", "interaction_requested", "fiber_exited"],
        ],
    );
    assert!(
        of_kind(&lines, "interaction_resolved").is_empty(),
        "the form stays pending"
    );
    assert!(
        of_kind(&lines, "tool_call_completed").is_empty(),
        "the call never completes"
    );
    let exited = of_kind(&lines, "fiber_exited")[0];
    assert_eq!(exited.payload["exit_code"], 143);
    assert_eq!(exited.payload["suspended_on"], request_id(&requested));
    assert_eq!(requested.payload["resumes"], true);
    assert_eq!(former.runs().len(), 1);
}

#[test]
fn a_reply_after_a_shutdown_answers_the_form_raised_again() {
    let former = Arc::new(Former::new());
    let (mut session, requested, _) = shut_down_on_form(&former);
    session.resume(vec![former.clone()]);
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    let raised = tap.wait_for("interaction_requested");
    assert_eq!(raised.payload, requested.payload, "the same request");
    let request = request_id(&raised);
    assert_eq!(answer(&session, &request, main_branch()), Ok(None));
    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            RERAISED,
            &["interaction_resolved", "tool_call_completed"],
            DONE,
        ],
    );
    assert_eq!(
        of_kind(&lines, "interaction_resolved")[0].payload["by"],
        "person"
    );
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["status"],
        "completed"
    );
    assert_eq!(former.runs().len(), 2);
}

/// Runs a session whose only call is `former`'s form until the idle delay
/// exits it, and returns it with the form's `interaction_requested`. The
/// lines through that line are read; the rest are not.
fn suspended_on_form(former: &Arc<Former>) -> (Session, Envelope) {
    let mut session = session(&["former"], vec![former.clone()], Some(IDLE));
    let tap = Tap::new(&session.log);
    let at = session.clock.now() + IDLE;
    let finished = start(&mut session);
    let requested = tap.wait_for("interaction_requested");
    assert!(session.clock.await_parked(at, DEADLINE), "the step idles");
    session.clock.advance(IDLE);
    assert_eq!(finish(&mut session, &finished), None);
    session.events_until("the form's request", |line| {
        line.kind == "interaction_requested"
    });
    (session, requested)
}

/// As [`suspended_on_form`], then `fiber_exited` and a resume with
/// `former`.
fn resumed_on_form(former: &Arc<Former>) -> (Session, Envelope) {
    let (mut session, requested) = suspended_on_form(former);
    exit(&mut session, None);
    session.resume(vec![former.clone()]);
    (session, requested)
}

/// The call that raised `requested`.
fn action_of(requested: &Envelope) -> String {
    requested.payload["action_ids"][0]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The finishing turn's lines up to its call's completion: a new process,
/// and the request raised again.
const RERAISED: &[&str] = &["fiber_started", "preamble_built", "interaction_requested"];

#[test]
fn a_reply_held_before_the_resume_answers_the_form_raised_again() {
    let former = Arc::new(Former::new());
    let (mut session, requested) = resumed_on_form(&former);
    let request = request_id(&requested);
    let replied = send_reply(&session, &request, main_branch());
    let finished = run_turn(&mut session);

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(replied.recv_timeout(DEADLINE).unwrap(), Ok(None));
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            RERAISED,
            &["interaction_resolved", "tool_call_completed"],
            DONE,
        ],
    );
    let raised = of_kind(&lines, "interaction_requested")[0];
    assert_eq!(raised.payload, requested.payload, "the same request");
    let resolved = of_kind(&lines, "interaction_resolved")[0];
    assert_eq!(resolved.payload["by"], "person");
    assert_eq!(resolved.payload["request_id"], request.as_str());
    let completed = of_kind(&lines, "tool_call_completed")[0];
    assert_eq!(completed.payload["status"], "completed");
    let runs = former.runs();
    assert_eq!(runs.len(), 2, "the call ran again");
    assert_eq!(runs[0], runs[1], "with the same arguments");
}

#[test]
fn the_rebuilt_conversation_holds_the_form_call_once_then_its_result() {
    let former = Arc::new(Former::new());
    let (mut session, requested) = resumed_on_form(&former);
    let replied = send_reply(&session, &request_id(&requested), main_branch());
    let finished = run_turn(&mut session);
    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(replied.recv_timeout(DEADLINE).unwrap(), Ok(None));

    let action = action_of(&requested);
    let request = session.requests().pop().unwrap();
    let calls: Vec<usize> = request
        .conversation
        .iter()
        .enumerate()
        .filter(|(_, input)| {
            matches!(input, Input::ToolCall { action_id, .. } if action_id.0 == action)
        })
        .map(|(at, _)| at)
        .collect();
    assert_eq!(calls.len(), 1, "the call once");
    match request.conversation.get(calls[0] + 1) {
        Some(Input::ToolResult {
            action_id, text, ..
        }) => {
            assert_eq!(action_id.0, action);
            assert_eq!(text, "{\"answers\":[{\"labels\":[\"main\"]}]}");
        }
        other => panic!("the call's result follows it, not {other:?}"),
    }
}

#[test]
fn a_reply_after_the_form_is_raised_again_answers_it() {
    let former = Arc::new(Former::new());
    let (mut session, requested) = resumed_on_form(&former);
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    let raised = tap.wait_for("interaction_requested");
    assert_eq!(raised.payload, requested.payload);
    let request = request_id(&raised);
    assert_eq!(answer(&session, &request, main_branch()), Ok(None));

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            RERAISED,
            &["interaction_resolved", "tool_call_completed"],
            DONE,
        ],
    );
    assert_eq!(former.runs().len(), 2);
}

#[test]
fn the_idle_delay_in_the_finishing_turn_suspends_on_the_same_request() {
    let former = Arc::new(Former::new());
    let (mut session, requested) = resumed_on_form(&former);
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.idle_exit(Some(IDLE)));
    let tap = Tap::new(&session.log);
    let at = session.clock.now() + IDLE;
    let finished = run_turn(&mut session);
    tap.wait_for("interaction_requested");
    idle_passes(&session, &tap, &finished, at);

    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, None);
    assert_kinds(&lines, &[RERAISED, &["fiber_exited"]]);
    assert_eq!(
        of_kind(&lines, "fiber_exited")[0].payload["suspended_on"],
        request_id(&requested)
    );
}

#[test]
fn a_shutdown_in_the_finishing_turn_suspends_on_the_same_request() {
    let former = Arc::new(Former::new());
    let (mut session, requested) = resumed_on_form(&former);
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.idle_exit(Some(IDLE)));
    let at = session.clock.now() + IDLE;
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    tap.wait_for("interaction_requested");
    // The raised-again line is written before the call's worker asks,
    // so only the step waiting on the pending form proves the call
    // asked and the loop pended it. The shutdown alone ends the step.
    assert!(
        session.clock.await_parked(at, DEADLINE),
        "the finishing step waits on the pending form"
    );
    session.cancel.shutdown(143);
    assert_eq!(finish(&mut session, &finished), None);
    let lines = exit(&mut session, Some(143));
    assert_kinds(&lines, &[RERAISED, &["fiber_exited"]]);
    assert!(
        of_kind(&lines, "interaction_resolved").is_empty(),
        "the form stays pending"
    );
    assert!(
        of_kind(&lines, "tool_call_completed").is_empty(),
        "the call never completes"
    );
    let exited = of_kind(&lines, "fiber_exited")[0];
    assert_eq!(exited.payload["suspended_on"], request_id(&requested));
    assert_eq!(exited.payload["exit_code"], 143);
    assert_eq!(former.runs().len(), 2);
}

#[test]
fn a_close_held_before_the_resume_ends_the_finishing_turn_with_the_questions() {
    let former = Arc::new(Former::new());
    let (mut session, _) = resumed_on_form(&former);
    let (ack, closed) = acked();
    session.inbox.send(Delivery::Close(ack)).unwrap();
    let finished = run_turn(&mut session);

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(closed.recv_timeout(DEADLINE).unwrap(), Ok(None));
    let lines = session.lines();
    ends_on_the_questions(&lines);
}

/// Asserts the finishing turn raised the request again, Fiber declined it,
/// and the turn ended on the call's questions.
fn ends_on_the_questions(lines: &[Envelope]) {
    assert_kinds(
        lines,
        &[
            RERAISED,
            &[
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    let resolved = of_kind(lines, "interaction_resolved")[0];
    assert_eq!(resolved.payload["by"], "fiber");
    assert_eq!(resolved.payload["declined"], true);
    let completed = of_kind(lines, "tool_call_completed")[0];
    assert_eq!(completed.payload["status"], "completed");
    assert_eq!(
        completed.payload["content"],
        json!([{"type": "text", "text": SENT}])
    );
    let asked = &completed.payload["control"]["questions"];
    assert_eq!(*asked, questions()["questions"]);
    let ended = of_kind(lines, "turn_completed")[0];
    assert_eq!(ended.payload["questions"], *asked);
}

#[test]
fn a_headless_resume_declines_the_form_and_runs_its_prompt_next() {
    let former = Arc::new(Former::new());
    let (mut session, _) = resumed_on_form(&former);
    session.looped = session.looped.take().map(|looped| looped.answerable(false));
    session.inbox.send(delivery("next")).unwrap();
    let finished = run_turn(&mut session);
    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    ends_on_the_questions(&session.lines());

    let finished = run_turn(&mut session);
    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_eq!(kinds(&lines)[0], "turn_started");
    assert_eq!(kinds(&lines)[1..], *DONE);
    assert_eq!(former.runs().len(), 2);
}

#[test]
fn a_cancel_in_the_finishing_turn_declines_the_form() {
    let former = Arc::new(Former::new());
    let (mut session, _) = resumed_on_form(&former);
    let tap = Tap::new(&session.log);
    let finished = run_turn(&mut session);
    tap.wait_for("interaction_requested");
    assert!(session.cancel.cancel());

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Interrupted)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[
            RERAISED,
            &[
                "interaction_resolved",
                "tool_call_completed",
                "turn_completed",
            ],
        ],
    );
    assert_eq!(
        of_kind(&lines, "interaction_resolved")[0].payload["by"],
        "fiber"
    );
    assert_eq!(
        of_kind(&lines, "tool_call_completed")[0].payload["status"],
        "cancelled"
    );
}

/// Appends a line of `kind` with `payload` under the suspended turn.
fn append(session: &Session, like: &Envelope, kind: &str, payload: Value, action: Option<&str>) {
    let mut line = like.clone();
    kind.clone_into(&mut line.kind);
    line.payload = payload.as_object().unwrap().clone();
    let event = Event::from_envelope(&line).unwrap().unwrap();
    let action = action.map(|action| ActionId(action.into()));
    session
        .log
        .append(&event, like.turn_id.clone(), action)
        .unwrap();
}

/// Suspends on a form, lets `edit` append lines and name the request
/// `fiber_exited` names, then resumes and runs a prompt: the session
/// resumes as cut short, and the form's call never runs again.
fn resumes_as_cut_short(edit: impl FnOnce(&Session, &Envelope) -> String) {
    let former = Arc::new(Former::new());
    let (mut session, requested) = suspended_on_form(&former);
    let named = edit(&session, &requested);
    let usage = json!({
        "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
        "cost": 0,
        "subscription_cost": 0,
    });
    let exited = json!({"exit_code": 0, "usage": usage, "suspended_on": named});
    let mut like = requested.clone();
    like.turn_id = None;
    append(&session, &like, "fiber_exited", exited, None);
    session.events_until("fiber_exited", |line| line.kind == "fiber_exited");
    session.resume(vec![former.clone()]);
    session.inbox.send(delivery("next")).unwrap();
    let finished = run_turn(&mut session);

    assert_eq!(
        finish(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_kinds(
        &lines,
        &[&["fiber_started", "preamble_built", "turn_started"], DONE],
    );
    assert!(
        of_kind(&lines, "interaction_requested").is_empty(),
        "nothing is raised again"
    );
    assert_eq!(former.runs().len(), 1, "the call never runs again");
}

/// A copy of `requested` under `id`, changed by `change`, appended.
fn copied(
    session: &Session,
    requested: &Envelope,
    id: &str,
    change: impl FnOnce(&mut Value),
) -> String {
    let mut payload = Value::Object(requested.payload.clone());
    payload["request_id"] = json!(id);
    change(&mut payload);
    append(session, requested, "interaction_requested", payload, None);
    id.to_owned()
}

#[test]
fn a_request_not_logged_resumes_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        copied(session, requested, "r_plain", |payload| {
            payload.as_object_mut().unwrap().remove("resumes");
        })
    });
}

#[test]
fn a_request_naming_two_calls_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        let action = action_of(requested);
        copied(session, requested, "r_two", |payload| {
            payload["action_ids"] = json!([action, "a_other"]);
        })
    });
}

#[test]
fn a_request_an_extension_raised_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        copied(session, requested, "r_ext", |payload| {
            payload["extension"] = json!("fiber.test/notes");
        })
    });
}

#[test]
fn a_resolved_request_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        let request = request_id(requested);
        let resolved = json!({"request_id": request, "by": "fiber", "declined": true});
        append(session, requested, "interaction_resolved", resolved, None);
        request
    });
}

#[test]
fn a_request_whose_call_completed_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        let action = action_of(requested);
        let completed = json!({"status": "completed", "content": []});
        append(
            session,
            requested,
            "tool_call_completed",
            completed,
            Some(&action),
        );
        request_id(requested)
    });
}

#[test]
fn a_request_whose_turn_completed_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        let ended = json!({"outcome": "interrupted"});
        append(session, requested, "turn_completed", ended, None);
        request_id(requested)
    });
}

#[test]
fn a_request_whose_call_never_started_is_not_raised_again() {
    resumes_as_cut_short(|session, requested| {
        let call = json!({"name": "former", "arguments": questions()});
        append(
            session,
            requested,
            "tool_call_requested",
            call,
            Some("a_unstarted"),
        );
        copied(session, requested, "r_unstarted", |payload| {
            payload["action_ids"] = json!(["a_unstarted"]);
        })
    });
}
