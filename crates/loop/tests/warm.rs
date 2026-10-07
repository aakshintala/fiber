//! Warming while idle (`docs/prompt-cache.md`, "Warming while idle"): with
//! `cache.warm_idle` set, the wait between turns resends the last step's
//! request with a one-token output cap 30 seconds before the cache lifetime
//! ends, until `cache.warm_cap` lifetimes after the last turn; then the idle
//! clock starts (`docs/invocation.md`, "Lifecycle").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::{Reply as Answer, ReplyAnswer};
use contract::events::{CacheLifetime, Decision};
use contract::inbox::{Ack, Delivery};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::provider::{CallError, Delta, ModelCall, ModelRequest, Provider, Reply};
use contract::shapes::Failure;
use contract::{Envelope, ErrorCode, JobId, RequestId};
use fakes::clock::FakeClock;
use fakes::{Scripted, ScriptedProvider, call_usage};

use support::{DEADLINE, Session, Tap, delivery};

const MINUTE: Duration = Duration::from_secs(60);

/// A session whose loop exits `idle` after warming stops and warms for
/// `cap` lifetimes.
fn session(script: Vec<Scripted>, lifetime: CacheLifetime) -> Session {
    Session::with_cache_lifetime(script, lifetime)
}

fn arm(session: &mut Session, idle: Duration, cap: Option<u32>) {
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.idle_exit(Some(idle)).warm(cap));
}

/// Runs `run` on its own thread and returns the channel it reports on.
fn spawn_run(session: &mut Session) -> mpsc::Receiver<Result<(), r#loop::Error>> {
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let result = looped.run();
        done.send(result).unwrap();
    });
    finished
}

fn parked(clock: &FakeClock, until: Instant, what: &str) {
    assert!(clock.await_parked(until, DEADLINE), "{what}");
}

/// Moves the clock to `to` and wakes the loop, as a real clock's timeout
/// would.
fn advance_to(session: &Session, to: Instant) {
    let now = session.clock.now();
    session.clock.advance(to.saturating_duration_since(now));
    wake(session);
}

/// Wakes the loop with a reply naming nothing, and returns once the loop
/// has taken it: its earlier park is over, so a later [`parked`] sees only
/// where it parks next.
fn wake(session: &Session) {
    let (done, taken) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reply(
            Answer {
                request_id: RequestId("r_absent".into()),
                answer: ReplyAnswer::Approval {
                    decision: Decision::Deny,
                    feedback: None,
                    remember: None,
                },
            },
            Ack(Box::new(move |_| {
                done.send(()).unwrap();
            })),
        ))
        .unwrap();
    taken
        .recv_timeout(DEADLINE)
        .expect("the loop took the wake");
}

fn ended(finished: &mpsc::Receiver<Result<(), r#loop::Error>>) {
    let ran = finished.recv_timeout(DEADLINE).expect("run ended in time");
    assert!(ran.is_ok(), "{ran:?}");
}

/// The durable lines after the last `turn_completed`.
fn after_turns(session: &Session) -> Vec<Envelope> {
    let lines = log::read(&session.dir).unwrap();
    let last = lines
        .iter()
        .rposition(|line| line.kind == "turn_completed")
        .expect("a turn ran");
    lines[last + 1..].to_vec()
}

/// `step` with its output capped at one token: what a refresh sends.
fn capped(step: &ModelRequest) -> ModelRequest {
    let mut request = step.clone();
    request.max_output_tokens = Some(1);
    request
}

/// A reply of `text` from generation `generation`, at an inline `cost`.
fn priced(text: &str, generation: &str, cost: f64) -> Scripted {
    let mut scripted = Scripted::text(text);
    if let Ok(reply) = &mut scripted.end {
        reply.generation_id = contract::GenerationId(generation.into());
        reply.cost = Some(cost);
    }
    scripted
}

fn refresh_reply() -> Scripted {
    Scripted::text("")
}

/// Every request after the first is the first, capped.
fn assert_refreshes(session: &Session, count: usize) {
    let requests = session.requests();
    assert_eq!(requests.len(), 1 + count, "{count} refreshes");
    assert_eq!(requests[0].max_output_tokens, None);
    for refresh in &requests[1..] {
        assert_eq!(*refresh, capped(&requests[0]));
    }
}

/// Each refresh's durable `usage_recorded`, in no turn and no action, and
/// nothing else after the turn.
fn assert_usage_only(session: &Session, count: usize) {
    let after = after_turns(session);
    assert_eq!(
        after.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        vec!["usage_recorded"; count]
    );
    for line in after {
        assert!(line.turn_id.is_none(), "a refresh belongs to no turn");
        assert!(line.action_id.is_none(), "a refresh belongs to no action");
        assert!(line.is_durable());
        assert_eq!(line.payload["input_bytes"], 1000);
        assert!(line.payload.get("input_media").is_none());
    }
}

#[test]
fn an_hour_lifetime_refreshes_twice_then_the_idle_clock_starts_at_the_cap() {
    let mut session = session(
        vec![Scripted::text("ok."), refresh_reply(), refresh_reply()],
        CacheLifetime::OneHour,
    );
    arm(&mut session, 30 * MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);

    let first = start + Duration::from_secs(3570);
    parked(&session.clock, first, "the first refresh is 30s before 1h");
    assert_refreshes(&session, 0);
    advance_to(&session, first);
    let second = first + Duration::from_secs(3570);
    parked(&session.clock, second, "the second counts from the first");
    assert_refreshes(&session, 1);
    advance_to(&session, second);
    // The third would come at 3 × 3570 s, past the cap of 7200 s.
    let exit = start + Duration::from_secs(7200) + 30 * MINUTE;
    parked(&session.clock, exit, "idle counts from the cap");
    assert_refreshes(&session, 2);
    advance_to(&session, start + Duration::from_millis(8_999_999));
    parked(&session.clock, exit, "not idle yet");
    assert!(finished.try_recv().is_err());
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 2);
    assert_usage_only(&session, 2);
}

#[test]
fn a_five_minute_lifetime_refreshes_at_270_and_540_seconds() {
    let mut session = session(
        vec![Scripted::text("ok."), refresh_reply(), refresh_reply()],
        CacheLifetime::FiveMinutes,
    );
    arm(&mut session, MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    for (at, count) in [(270, 0), (540, 1)] {
        let due = start + Duration::from_secs(at);
        parked(&session.clock, due, "a refresh is due");
        assert_refreshes(&session, count);
        advance_to(&session, due);
    }
    let exit = start + Duration::from_secs(600) + MINUTE;
    parked(&session.clock, exit, "idle counts from 600 s");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 2);
}

#[test]
fn warming_off_or_a_cap_of_zero_sends_nothing_after_the_turn() {
    for cap in [None, Some(0)] {
        let mut session = session(
            vec![Scripted::text("ok."), refresh_reply()],
            CacheLifetime::FiveMinutes,
        );
        arm(&mut session, MINUTE, cap);
        let start = session.clock.now();
        session.inbox.send(delivery("hi")).unwrap();
        let finished = spawn_run(&mut session);
        let exit = start + MINUTE;
        parked(&session.clock, exit, "idle counts from the wait start");
        advance_to(&session, exit);
        ended(&finished);
        assert_refreshes(&session, 0);
        assert_usage_only(&session, 0);
    }
}

#[test]
fn a_refresh_due_at_the_cap_is_not_sent() {
    // The step's call takes 240 s, so the wait starts at 240 s and the cap of
    // one 5-minute lifetime ends at 540 s: the second refresh, due at 540 s,
    // is not before it.
    let mut session = advancing(Duration::from_secs(240));
    arm(&mut session, MINUTE, Some(1));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let first = start + Duration::from_secs(270);
    parked(&session.clock, first, "the first refresh");
    advance_to(&session, first);
    let exit = start + Duration::from_secs(540) + MINUTE;
    parked(&session.clock, exit, "warming stopped at the cap");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 1);
}

#[test]
fn a_refresh_reached_only_at_the_cap_is_not_sent() {
    // The wait starts at 250 s; the cap ends at 550 s. The second refresh is
    // due at 540 s, but the loop gets to it only at 550 s, while the cache
    // from the 270 s refresh still holds.
    let mut session = advancing(Duration::from_secs(250));
    arm(&mut session, MINUTE, Some(1));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let first = start + Duration::from_secs(270);
    parked(&session.clock, first, "the first refresh");
    advance_to(&session, first);
    parked(
        &session.clock,
        start + Duration::from_secs(540),
        "the second is due before the cap",
    );
    let late = start + Duration::from_secs(550);
    advance_to(&session, late);
    let exit = late + MINUTE;
    parked(&session.clock, exit, "warming stopped at the cap");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 1);
}

#[test]
fn a_spent_budget_stops_warming_and_fails_no_turn() {
    let mut session = session(
        vec![priced("ok.", "g_1", 1.0), refresh_reply()],
        CacheLifetime::FiveMinutes,
    )
    .budget(Some(1.0));
    arm(&mut session, MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let due = start + Duration::from_secs(270);
    parked(&session.clock, due, "the refresh is due");
    advance_to(&session, due);
    let exit = due + MINUTE;
    parked(&session.clock, exit, "idle counts from the refused refresh");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 0);
    assert_usage_only(&session, 0);
}

#[test]
fn a_refresh_s_spend_counts_toward_the_budget() {
    let mut session = session(
        vec![
            priced("ok.", "g_1", 0.4),
            priced("", "g_2", 0.6),
            refresh_reply(),
        ],
        CacheLifetime::FiveMinutes,
    )
    .budget(Some(1.0));
    arm(&mut session, MINUTE, Some(3));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let first = start + Duration::from_secs(270);
    parked(&session.clock, first, "the first refresh");
    advance_to(&session, first);
    let second = start + Duration::from_secs(540);
    parked(&session.clock, second, "the second is due");
    advance_to(&session, second);
    let exit = second + MINUTE;
    parked(&session.clock, exit, "the first refresh spent the budget");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 1);
    let after = after_turns(&session);
    assert_eq!(after[0].payload["cost"], 0.6);
}

#[test]
fn a_prompt_while_warming_starts_a_turn_and_the_cap_counts_from_the_next_wait() {
    let mut session = session(
        vec![
            Scripted::text("ok."),
            Scripted::text("again."),
            refresh_reply(),
            refresh_reply(),
        ],
        CacheLifetime::FiveMinutes,
    );
    arm(&mut session, MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    parked(
        &session.clock,
        start + Duration::from_secs(270),
        "warming after the first turn",
    );
    session.clock.advance(Duration::from_secs(100));
    session.inbox.send(delivery("again")).unwrap();
    let second = start + Duration::from_secs(100);
    for at in [370, 640] {
        let due = start + Duration::from_secs(at);
        parked(&session.clock, due, "warming counts from the second turn");
        advance_to(&session, due);
    }
    // The cap ends 600 s after the second wait began, at 700 s.
    let exit = second + Duration::from_secs(600) + MINUTE;
    parked(&session.clock, exit, "idle counts from the second cap");
    advance_to(&session, exit);
    ended(&finished);
    let requests = session.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2], capped(&requests[1]));
    assert_eq!(requests[3], capped(&requests[1]));
}

#[test]
fn a_failed_refresh_is_not_retried_and_stops_warming_with_a_notice() {
    let mut session = session(
        vec![
            Scripted::text("ok."),
            Scripted::failed(Failure {
                code: ErrorCode::ProviderUnavailable,
                message: "Overloaded.".into(),
                retry_after_ms: None,
                provider: None,
            }),
            refresh_reply(),
        ],
        CacheLifetime::FiveMinutes,
    );
    arm(&mut session, MINUTE, Some(2));
    let tap = Tap::new(&session.log);
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let due = start + Duration::from_secs(270);
    parked(&session.clock, due, "the refresh is due");
    advance_to(&session, due);
    let notice = tap.wait_for("notice");
    assert_eq!(notice.payload["code"], "provider_unavailable");
    assert!(
        notice.payload["message"]
            .as_str()
            .unwrap()
            .contains("Overloaded."),
        "{notice:?}"
    );
    assert!(!notice.is_durable(), "the notice is ephemeral");
    assert!(notice.turn_id.is_none());
    let exit = due + MINUTE;
    parked(&session.clock, exit, "idle counts from the failure");
    advance_to(&session, exit);
    ended(&finished);
    // One failed refresh, no retry.
    assert_eq!(session.requests().len(), 2);
    assert_usage_only(&session, 0);
}

/// Jobs a test turns on and off: one job runs while `running` is set.
#[derive(Default)]
struct Toggle {
    running: AtomicBool,
}

impl Toggle {
    fn set(&self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
    }
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

#[test]
fn no_refresh_while_a_job_runs_and_one_at_once_when_it_ends_in_time() {
    let mut session = session(
        vec![Scripted::text("ok."), refresh_reply(), refresh_reply()],
        CacheLifetime::FiveMinutes,
    );
    let jobs = Arc::new(Toggle::default());
    jobs.set(true);
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.jobs(Arc::clone(&jobs) as Arc<dyn Jobs>));
    let idle = 30 * MINUTE;
    arm(&mut session, idle, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    // While a job runs the wait keeps only the jobs check, from the prompt.
    let check = start + idle;
    parked(&session.clock, check, "a running job holds the refresh");
    advance_to(&session, start + Duration::from_secs(280));
    parked(&session.clock, check, "still no refresh at 280 s");
    assert_refreshes(&session, 0);

    // The job ends before the cache does: the late refresh goes at once.
    jobs.set(false);
    wake(&session);
    let next = start + Duration::from_secs(550);
    parked(
        &session.clock,
        next,
        "the next counts from the late refresh",
    );
    assert_refreshes(&session, 1);

    // A job runs across that due instant and ends as the cache expires.
    jobs.set(true);
    let expired = start + Duration::from_secs(580);
    advance_to(&session, expired);
    parked(&session.clock, check, "a running job holds the refresh");
    jobs.set(false);
    wake(&session);
    let exit = expired + idle;
    parked(&session.clock, exit, "an expired cache stops warming");
    advance_to(&session, exit);
    ended(&finished);
    assert_refreshes(&session, 1);
}

#[test]
fn a_shutdown_while_waiting_for_a_refresh_ends_run() {
    let mut session = session(
        vec![Scripted::text("ok."), refresh_reply()],
        CacheLifetime::FiveMinutes,
    );
    arm(&mut session, MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    parked(
        &session.clock,
        start + Duration::from_secs(270),
        "waiting to refresh",
    );
    session.cancel.shutdown(130);
    session.inbox.send(Delivery::Cancelled).unwrap();
    ended(&finished);
    assert_refreshes(&session, 0);
}

#[test]
fn a_shutdown_cancels_a_refresh_in_flight_and_records_no_usage() {
    let started = Arc::new(Mutex::new(None));
    let signal = Arc::clone(&started);
    let mut session = Session::wrapped(
        vec![Scripted::text("ok."), refresh_reply()],
        CacheLifetime::FiveMinutes,
        move |inner, _clock| {
            let (tx, rx) = mpsc::channel();
            *signal.lock().unwrap() = Some(rx);
            Arc::new(BlockSecond {
                inner,
                calls: AtomicUsize::new(0),
                started: tx,
            })
        },
    );
    arm(&mut session, MINUTE, Some(2));
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let due = start + Duration::from_secs(270);
    parked(&session.clock, due, "waiting to refresh");
    advance_to(&session, due);
    let blocked = started.lock().unwrap().take().unwrap();
    blocked
        .recv_timeout(DEADLINE)
        .expect("the refresh is in flight");
    session.cancel.shutdown(130);
    ended(&finished);
    assert_eq!(session.requests().len(), 2);
    assert_usage_only(&session, 0);
}

/// A provider whose second call blocks until cancelled, signalling when it
/// starts; every other call is the scripted one.
struct BlockSecond {
    inner: Arc<ScriptedProvider>,
    calls: AtomicUsize,
    started: mpsc::Sender<()>,
}

impl Provider for BlockSecond {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let call = self.inner.call(request);
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            let (cancel, cancelled) = mpsc::channel();
            Box::new(Blocked {
                started: self.started.clone(),
                cancel,
                cancelled: Mutex::new(cancelled),
            })
        } else {
            call
        }
    }
}

struct Blocked {
    started: mpsc::Sender<()>,
    cancel: mpsc::Sender<()>,
    cancelled: Mutex<mpsc::Receiver<()>>,
}

impl ModelCall for Blocked {
    fn run(&self, _sink: &mut dyn FnMut(Delta)) -> Result<Reply, CallError> {
        self.started.send(()).unwrap();
        let got = self.cancelled.lock().unwrap().recv_timeout(DEADLINE);
        assert!(got.is_ok(), "the shutdown cancels the refresh");
        Err(CallError::Cancelled { usage: None })
    }

    fn cancel(&self) {
        let _sent = self.cancel.send(()).is_ok();
    }
}

/// A 5-minute session whose first model call moves the clock on by `by`
/// while it runs: the step is stamped before it, so the idle wait starts
/// `by` after the send.
fn advancing(by: Duration) -> Session {
    Session::wrapped(
        vec![Scripted::text("ok."), refresh_reply(), refresh_reply()],
        CacheLifetime::FiveMinutes,
        move |inner, clock| {
            Arc::new(Advance {
                inner,
                clock,
                by,
                calls: AtomicUsize::new(0),
            })
        },
    )
}

struct Advance {
    inner: Arc<ScriptedProvider>,
    clock: Arc<FakeClock>,
    by: Duration,
    calls: AtomicUsize,
}

impl Provider for Advance {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.clock.advance(self.by);
        }
        self.inner.call(request)
    }
}

#[test]
fn a_failed_refresh_after_its_generation_writes_its_usage_then_the_notice() {
    let mut session = session(
        vec![
            Scripted::text("ok."),
            Scripted::failed_after(
                Failure {
                    code: ErrorCode::ProviderUnavailable,
                    message: "Overloaded.".into(),
                    retry_after_ms: None,
                    provider: None,
                },
                call_usage("gen_refresh"),
            ),
            refresh_reply(),
        ],
        CacheLifetime::FiveMinutes,
    );
    arm(&mut session, MINUTE, Some(2));
    let mut watcher = session.log.watch();
    let start = session.clock.now();
    session.inbox.send(delivery("hi")).unwrap();
    let finished = spawn_run(&mut session);
    let due = start + Duration::from_secs(270);
    parked(&session.clock, due, "the refresh is due");
    advance_to(&session, due);
    // The lines after the turn are exactly `usage_recorded, notice`, in
    // order: consume to the turn's end, skipping the status observer's
    // lines, then read the next two.
    let mut next = || {
        loop {
            let line = watcher
                .recv_timeout(DEADLINE)
                .expect("a line in time")
                .expect("the log outlives the turn")
                .expect("the log ended before the awaited line");
            if line.kind != "session_status" {
                return line;
            }
        }
    };
    loop {
        if next().kind == "turn_completed" {
            break;
        }
    }
    let usage = next();
    assert_eq!(usage.kind, "usage_recorded");
    assert!(usage.turn_id.is_none(), "a refresh belongs to no turn");
    assert!(usage.action_id.is_none(), "a refresh belongs to no action");
    assert_eq!(usage.payload["generation_id"], "gen_refresh");
    let notice = next();
    assert_eq!(notice.kind, "notice");
    assert_eq!(notice.payload["code"], "provider_unavailable");
    assert!(!notice.is_durable(), "the notice is ephemeral");
    assert!(notice.turn_id.is_none());
    let exit = due + MINUTE;
    parked(&session.clock, exit, "idle counts from the failure");
    advance_to(&session, exit);
    ended(&finished);
    assert_eq!(session.requests().len(), 2);
    let after = after_turns(&session);
    assert_eq!(
        after.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        vec!["usage_recorded"]
    );
}
