//! The `session_status` a running loop writes about itself
//! (`docs/events.md`, `session_status`): through the loop's public API,
//! against a scripted provider.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use contract::Envelope;
use contract::JobId;
use contract::events::{JobCompleted, Outcome};
use contract::inbox::{Ack, Claim, Delivery, JobNotice};
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::provider::Provider;
use contract::tool::Tool;
use fakes::Deadline;
use fakes::Scripted;
use log::Watcher;
use r#loop::{Loop, Model};

use support::{DEADLINE, MODEL, Session, TestTool, delivery, ignore, tool_call_reply};

/// The `session_status` lines from now on, up to and including the first one
/// `until` accepts, with one [`DEADLINE`]. Takes the watcher and returns it
/// with the lines.
fn statuses_until(
    watcher: Watcher,
    until: impl Fn(&Envelope) -> bool + Send + 'static,
) -> (Watcher, Vec<Envelope>) {
    let (watcher, lines) = support::read_until(watcher, "a session_status line", move |line| {
        line.kind == "session_status" && until(line)
    });
    let found: Vec<Envelope> = lines
        .into_iter()
        .filter(|line| line.kind == "session_status")
        .collect();
    (watcher, found)
}

fn idle_named(name: &'static str) -> impl Fn(&Envelope) -> bool {
    move |line| line.payload["state"] == "idle" && line.payload["name"] == name
}

/// Runs `looped` on its own thread; the channel reports its end.
fn run(looped: Loop) -> Receiver<Result<(), r#loop::Error>> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run().result).unwrap());
    finished
}

fn states(lines: &[Envelope]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| line.payload["state"].as_str().unwrap())
        .collect()
}

#[test]
fn a_running_session_writes_its_status_at_the_start_and_at_each_change() {
    let weather = Arc::new(TestTool::reads("weather", "sunny"));
    let mut session = Session::with_tools(
        vec![tool_call_reply("", &["weather"]), Scripted::text("Done.")],
        None,
        vec![weather as Arc<dyn Tool>],
    );
    let watcher = session.log.watch();
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let (_watcher, seen) = statuses_until(watcher, idle_named("say hi"));
    // The first status is written before any work, and still includes turns at zero.
    assert_eq!(seen[0].payload["state"], "idle");
    assert_eq!(seen[0].payload["name"], "");
    assert_eq!(seen[0].payload["turns"], 0);
    assert_eq!(seen.last().unwrap().payload["turns"], 1);
    let order = states(&seen);
    assert_eq!(order.last(), Some(&"idle"));
    let streaming = order.iter().position(|s| *s == "streaming").unwrap();
    let tool = order.iter().position(|s| *s == "tool").unwrap();
    assert!(streaming < tool);
    let tool_line = seen.iter().find(|s| s.payload["state"] == "tool").unwrap();
    assert_eq!(tool_line.payload["tool"], "weather");
    // Only changes are written.
    for pair in seen.windows(2) {
        assert_ne!(pair[0].payload, pair[1].payload);
    }
    // Ephemeral: no `seq`, a session id, and nothing in the log file.
    for line in &seen {
        assert_eq!(line.seq, None);
        assert_eq!(line.session_id.0, "s_test");
        assert_eq!(
            line.payload["workspace"],
            session.workspace.display().to_string()
        );
        assert_eq!(line.payload["model"], MODEL);
        assert!(!line.payload.contains_key("thinking"));
        assert_eq!(line.payload["delegates"], 0);
        assert_eq!(line.payload["jobs"], 0);
    }
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop")
        .unwrap();
    let kinds: Vec<String> = log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(!kinds.iter().any(|k| k == "session_status"));
}

#[test]
fn a_session_with_a_thinking_level_names_it_in_its_status() {
    let mut session = Session::with_thinking(
        vec![Scripted::text("Done.")],
        Some(contract::ThinkingLevel::High),
    );
    let watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let (_watcher, seen) = statuses_until(watcher, idle_named("go"));
    assert_eq!(seen.last().unwrap().payload["thinking"], "high");
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop")
        .unwrap();
}

#[test]
fn two_calls_report_their_classified_subject_and_session_start() {
    let weather = Arc::new(TestTool::reads("weather", "sunny"));
    let mut session = Session::with_tools(
        vec![
            tool_call_reply("", &["weather"]),
            tool_call_reply("", &["shell"]),
            Scripted::text("Done."),
        ],
        None,
        vec![
            weather as Arc<dyn Tool>,
            support::shell(Some("ls -la"), None),
        ],
    );
    session.rules.set(contract::rules::StandingRules {
        global: vec![contract::rules::Rule {
            decision: contract::rules::RuleDecision::Allow,
            tool: "shell".into(),
            prefix: "ls".into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    });
    let started = log::read(&session.dir)
        .unwrap()
        .into_iter()
        .find(|line| line.kind == "session_started")
        .unwrap()
        .ts;
    let watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let (_watcher, seen) = statuses_until(watcher, |line| {
        line.payload["calls"] == 2 && line.payload["state"] == "idle"
    });
    assert_eq!(seen[0].payload["calls"], 0);
    assert!(!seen[0].payload.contains_key("step"));
    let first = seen.iter().find(|line| line.payload["calls"] == 1).unwrap();
    assert_eq!(first.payload["step"], "weather");
    assert_eq!(seen.last().unwrap().payload["step"], "shell ls -la");
    for line in seen {
        assert_eq!(line.payload["started"], started);
    }
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop")
        .unwrap();
}

#[test]
fn a_session_that_accepts_close_says_so_in_its_status() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let (_watcher, seen) = statuses_until(watcher, idle_named("go"));
    assert!(
        seen.iter()
            .all(|line| !line.payload.contains_key("closing")),
        "no status carries closing before the close"
    );
    let log = Arc::clone(&session.log);
    let (ack_tx, ack_rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Close(Ack(Box::new(move |answer| {
            let latest = log.latest("session_status").map(|line| line.payload);
            ack_tx.send((answer.is_ok(), latest)).unwrap();
        }))))
        .unwrap();
    let (ok, latest) = Deadline::after(DEADLINE)
        .recv(&ack_rx)
        .expect("the close is acknowledged after the status is published");
    assert!(ok, "the close is accepted");
    let payload = latest.expect("a session_status is published");
    assert_eq!(payload["closing"].as_bool(), Some(true));
    assert_eq!(payload["state"].as_str(), Some("idle"));
    assert_eq!(payload["name"].as_str(), Some("go"));
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop")
        .unwrap();
}

/// Jobs a test holds from the status observer: `running` lists one job
/// while `on`, and the first call on the observer's thread after `arm`
/// signals it is held and waits for the release, so the status fold
/// pauses mid-line.
struct HeldJobs {
    on: AtomicBool,
    armed: AtomicBool,
    held: Mutex<Option<mpsc::Sender<()>>>,
    released: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Jobs for HeldJobs {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError {
            path: std::path::PathBuf::from("jobs"),
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
        if !self.on.load(Ordering::SeqCst) {
            return Vec::new();
        }
        if self.armed.load(Ordering::SeqCst)
            && std::thread::current().name() == Some("status")
            && let Some(held) = self.held.lock().unwrap().take()
        {
            let released = self.released.lock().unwrap().take().unwrap();
            held.send(()).unwrap();
            // The test's own wait fails first and drops the sender, which
            // releases this hold.
            match released.recv() {
                Ok(()) | Err(_) => {}
            }
        }
        vec![JobId("j_1".into())]
    }

    fn deliver_to(&self, _inbox: mpsc::Sender<Delivery>) {}
}

#[test]
fn a_new_subscriber_reads_closing_after_the_close_is_accepted() {
    let mut session = Session::with_tools(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        None,
        Vec::new(),
    );
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let jobs = Arc::new(HeldJobs {
        on: AtomicBool::new(true),
        armed: AtomicBool::new(false),
        held: Mutex::new(Some(held_tx)),
        released: Mutex::new(Some(release_rx)),
    });
    let looped = session.looped.take().unwrap();
    session.looped = Some(looped.jobs(Arc::clone(&jobs) as Arc<dyn Jobs>));
    let watcher = session.log.watch();
    let finished = run(session.looped.take().unwrap());
    let (watcher, seen) = statuses_until(watcher, |_| true);
    assert!(!seen[0].payload.contains_key("closing"));
    jobs.armed.store(true, Ordering::SeqCst);
    session.inbox.send(delivery("go")).unwrap();
    Deadline::after(DEADLINE)
        .recv(&held_rx)
        .expect("the observer is held folding turn_started");
    // The turn still completes: durable lines reach the watcher whatever
    // the observer is doing.
    let (_watcher, _) = support::read_until(watcher, "the prompt's turn_completed", |line| {
        line.kind == "turn_completed"
    });
    release_tx.send(()).unwrap();
    let log = Arc::clone(&session.log);
    let (ack_tx, ack_rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Close(Ack(Box::new(move |answer| {
            let latest = log.latest("session_status").map(|line| line.payload);
            ack_tx.send((answer.is_ok(), latest)).unwrap();
        }))))
        .unwrap();
    let (ok, latest) = Deadline::after(DEADLINE)
        .recv(&ack_rx)
        .expect("the close is acknowledged");
    assert!(ok, "the close is accepted");
    let payload = latest.expect("a session_status is published");
    assert_eq!(payload["closing"].as_bool(), Some(true));
    assert_eq!(payload["name"].as_str(), Some("go"));
    // A subscriber that attaches mid-close reads closing in the first
    // status it is sent, as `crates/doors/src/client.rs` does for `summary`.
    let _second = session.log.watch();
    let latest = session
        .log
        .latest("session_status")
        .expect("a session_status");
    assert_eq!(latest.payload["closing"].as_bool(), Some(true));
    jobs.on.store(false, Ordering::SeqCst);
    session
        .inbox
        .send(Delivery::Job(JobNotice {
            completed: JobCompleted {
                job_id: JobId("j_1".into()),
                status: Outcome::Completed,
                error: None,
                process: None,
                output_tail: None,
                notice: None,
                changed_by: None,
            },
            claim: Claim(Box::new(|| Some(Vec::new()))),
            delegate: None,
        }))
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the loop ended")
        .unwrap();
}

#[test]
fn no_status_follows_the_line_written_after_run_returns() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    let (watcher, seen) = statuses_until(watcher, idle_named("go"));
    assert_eq!(seen.last().unwrap().payload["state"], "idle");
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    let ran = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop");
    ran.result.unwrap();
    // As `fiber ask` does: `fiber_exited` after `run` returns.
    r#loop::fiber_exited(&session.log, &ran.summary, Ok(()), true, None).unwrap();
    let (mut watcher, lines) = support::read_until(watcher, "a fiber_exited line", |line| {
        line.kind == "fiber_exited"
    });
    let statuses: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "session_status")
        .collect();
    assert!(
        !statuses.is_empty(),
        "the accepted close writes one last status"
    );
    assert_eq!(
        statuses.last().unwrap().payload["closing"].as_bool(),
        Some(true),
        "the last status carries the accepted close"
    );
    assert!(
        lines
            .iter()
            .rposition(|line| line.kind == "fiber_exited")
            .unwrap()
            > lines
                .iter()
                .rposition(|line| line.kind == "session_status")
                .unwrap(),
        "no status after fiber_exited"
    );
    assert!(
        watcher.try_recv().unwrap().is_none(),
        "fiber_exited is the last line"
    );
}

#[test]
fn a_resumed_session_writes_one_status_for_its_history() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let first = session.log.watch();
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let (_first, _seen) = statuses_until(first, idle_named("say hi"));
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("close ended the loop")
        .unwrap();

    let lines = session.log.watch();
    let (inbox, rx) = mpsc::channel::<Delivery>();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let resumed = Loop::resume(
        r#loop::Session {
            log: Arc::clone(&session.log),
            provider: Arc::clone(&session.provider) as Arc<dyn Provider>,
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt: r#loop::PromptInputs::new(
                session.dir.clone(),
                "/bin/sh".into(),
                session.dir.join("events.jsonl").display().to_string(),
                clock,
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: Vec::new(),
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        r#loop::resumed(&session.dir).unwrap(),
    )
    .unwrap();
    let finished = run(resumed);
    let (mut lines, seen) = statuses_until(lines, |_| true);
    assert_eq!(seen[0].payload["name"], "say hi");
    assert_eq!(seen[0].payload["turns"], 1);
    assert_eq!(seen[0].payload["state"], "idle");
    drop(inbox);
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the loop ended with its inbox")
        .unwrap();
    // One status for the whole history, and none after it.
    loop {
        match lines.try_recv() {
            Ok(Some(line)) => assert_ne!(
                line.kind, "session_status",
                "only one status for the whole history"
            ),
            Ok(None) => break,
            Err(e) => panic!("the resumed watcher failed: {e}"),
        }
    }
}
