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
use std::sync::mpsc::{self, Receiver};
use std::thread;

use std::collections::BTreeMap;

use contract::events::{
    Empty, Event, InputItem, TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded,
};
use contract::inbox::Delivery;
use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
use contract::provider::Provider;
use contract::shapes::{ContentPart, Origin, Sender, Tokens};
use contract::tool::Tool;
use contract::{CommandId, Envelope, GenerationId, JobId, TurnId};
use fakes::Scripted;
use log::Log;
use r#loop::{Loop, Model};

use support::{DEADLINE, Gate, MODEL, Session, TestTool, delivery, ignore, tool_call_reply};

/// Every line the log emits from now on, on a channel: the watcher blocks
/// without a deadline, so each receive below carries [`DEADLINE`].
fn tap(log: &Log) -> Receiver<Envelope> {
    let mut watcher = log.watch();
    let (forward, lines) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    lines
}

/// The `session_status` lines on `lines`, up to and including the first one
/// `until` accepts.
fn statuses_until(lines: &Receiver<Envelope>, until: impl Fn(&Envelope) -> bool) -> Vec<Envelope> {
    let mut found = Vec::new();
    loop {
        let line = lines
            .recv_timeout(DEADLINE)
            .expect("a session_status the test waits for");
        if line.kind != "session_status" {
            continue;
        }
        let done = until(&line);
        found.push(line);
        if done {
            return found;
        }
    }
}

fn idle_named(name: &'static str) -> impl Fn(&Envelope) -> bool {
    move |line| line.payload["state"] == "idle" && line.payload["name"] == name
}

/// Runs `looped` on its own thread; the channel reports its end.
fn run(looped: Loop) -> Receiver<Result<(), r#loop::Error>> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
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
    let lines = tap(&session.log);
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let seen = statuses_until(&lines, idle_named("say hi"));
    // The first status is the one written at the start: nothing has run.
    assert_eq!(seen[0].payload["state"], "idle");
    assert_eq!(seen[0].payload["name"], "");
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
        assert_eq!(line.payload["delegates"], 0);
        assert_eq!(line.payload["jobs"], 0);
    }
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
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
fn no_status_follows_the_line_written_after_run_returns() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let lines = tap(&session.log);
    session.inbox.send(delivery("go")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let seen = statuses_until(&lines, idle_named("go"));
    assert_eq!(seen.last().unwrap().payload["state"], "idle");
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    // As `fiber ask` does: `fiber_exited` after `run` returns.
    r#loop::fiber_exited(&session.log, &session.dir, Ok(())).unwrap();
    loop {
        let line = lines.recv_timeout(DEADLINE).expect("fiber_exited arrives");
        assert_ne!(
            line.kind, "session_status",
            "no status after the loop ended"
        );
        if line.kind == "fiber_exited" {
            break;
        }
    }
    assert!(lines.try_recv().is_err(), "fiber_exited is the last line");
}

#[test]
fn a_resumed_session_writes_one_status_for_its_history() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let first = tap(&session.log);
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    statuses_until(&first, idle_named("say hi"));
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
        .expect("close ended the loop")
        .unwrap();

    let history = log::read(&session.dir).unwrap();
    let lines = tap(&session.log);
    let (inbox, rx) = mpsc::channel::<Delivery>();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let resumed = Loop::resume(
        Arc::clone(&session.log),
        &history,
        Arc::clone(&session.provider) as Arc<dyn Provider>,
        Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        r#loop::PromptInputs::new(
            session.dir.clone(),
            "/bin/sh".into(),
            session.dir.join("events.jsonl").display().to_string(),
            clock,
        ),
        rx,
        Vec::new(),
        r#loop::Permissions {
            workspace: session.workspace.display().to_string(),
            credentials: session.credentials.clone(),
            rules: session.rules.clone(),
        },
    )
    .unwrap();
    let finished = run(resumed);
    let seen = statuses_until(&lines, |_| true);
    assert_eq!(seen[0].payload["name"], "say hi");
    assert_eq!(seen[0].payload["state"], "idle");
    drop(inbox);
    finished
        .recv_timeout(DEADLINE)
        .expect("the loop ended with its inbox")
        .unwrap();
    // One status for the whole history, and none after it.
    assert!(lines.try_iter().all(|line| line.kind != "session_status"));
}

/// A job registry whose `running` waits at a gate: the observer, which reads
/// it as it goes live, cannot read its queue until the test opens the gate.
struct Gated(Arc<Gate>);

impl Jobs for Gated {
    fn open(&self, _: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: "unused".into(),
            source: std::io::Error::other("a gated registry opens nothing"),
        })
    }

    fn stop(&self, _: &JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _: Foreground) {}

    fn running(&self) -> Vec<JobId> {
        self.0.wait();
        Vec::new()
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

/// The observer's queue overflows while it is held, and the stop line, which
/// is kept, comes before the durable lines the queue dropped. The observer
/// still folds those lines before it ends: the last status is the final one.
#[test]
fn a_lagging_observer_folds_every_written_line_before_it_stops() {
    let gate = Arc::new(Gate::default());
    let mut session = Session::new(Vec::new(), None);
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.jobs(Arc::new(Gated(Arc::clone(&gate)))));
    let lines = tap(&session.log);
    let finished = run(session.looped.take().unwrap());

    let append = |event: Event| {
        session
            .log
            .append(&event, Some(TurnId("t_x".into())), None)
            .unwrap();
    };
    append(Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "go".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    }));
    // More lines than a watcher's queue holds, so the rest are dropped.
    for _ in 0..2_000 {
        append(Event::StepStarted(Empty {}));
    }
    append(Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId("g_1".into()),
        model: MODEL.into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 3,
        },
        web_searches: None,
        cost: Some(1.5),
        subscription: None,
        extension: None,
        origin_session_id: None,
    }));
    append(Event::TurnCompleted(TurnCompleted {
        outcome: TurnOutcome::Completed,
        error: None,
        questions: None,
    }));

    gate.open();
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    gate.check("the observer's registry read");

    let last = lines
        .try_iter()
        .filter(|line| line.kind == "session_status")
        .last()
        .expect("a session_status");
    assert_eq!(last.payload["state"], "idle");
    assert_eq!(last.payload["name"], "go");
    assert_eq!(last.payload["spend"]["cost"], 1.5);
    assert_eq!(last.payload["spend"]["tokens"]["input"], 10);
}
