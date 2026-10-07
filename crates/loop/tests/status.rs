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

use contract::Envelope;
use contract::inbox::Delivery;
use contract::provider::Provider;
use contract::tool::Tool;
use fakes::Scripted;
use log::Watcher;
use r#loop::{Loop, Model};

use support::{DEADLINE, MODEL, Session, TestTool, delivery, ignore, tool_call_reply};

/// The `session_status` lines from now on, up to and including the first one
/// `until` accepts. Each wait carries [`DEADLINE`].
fn statuses_until(watcher: &mut Watcher, until: impl Fn(&Envelope) -> bool) -> Vec<Envelope> {
    let mut found = Vec::new();
    loop {
        let line = watcher
            .recv_timeout(DEADLINE)
            .expect("a session_status the test waits for in time")
            .expect("the log outlives the status")
            .expect("the log ended before the awaited session_status");
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
    let mut watcher = session.log.watch();
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let seen = statuses_until(&mut watcher, idle_named("say hi"));
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
    let mut watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let finished = run(session.looped.take().unwrap());
    let seen = statuses_until(&mut watcher, idle_named("go"));
    assert_eq!(seen.last().unwrap().payload["state"], "idle");
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
        .expect("close ended the loop")
        .unwrap();
    // As `fiber ask` does: `fiber_exited` after `run` returns.
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), true, None).unwrap();
    loop {
        let line = watcher
            .recv_timeout(DEADLINE)
            .expect("fiber_exited arrives in time")
            .expect("the log outlives fiber_exited")
            .expect("the log ended before fiber_exited");
        assert_ne!(
            line.kind, "session_status",
            "no status after the loop ended"
        );
        if line.kind == "fiber_exited" {
            break;
        }
    }
    assert!(
        watcher.try_recv().unwrap().is_none(),
        "fiber_exited is the last line"
    );
}

#[test]
fn a_resumed_session_writes_one_status_for_its_history() {
    let mut session = Session::new(vec![Scripted::text("Hi.")], None);
    let mut first = session.log.watch();
    session.inbox.send(delivery("say hi")).unwrap();
    let finished = run(session.looped.take().unwrap());
    statuses_until(&mut first, idle_named("say hi"));
    session.inbox.send(Delivery::Close(ignore())).unwrap();
    finished
        .recv_timeout(DEADLINE)
        .expect("close ended the loop")
        .unwrap();

    let mut lines = session.log.watch();
    let (inbox, rx) = mpsc::channel::<Delivery>();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let resumed = Loop::resume(
        Arc::clone(&session.log),
        r#loop::resumed(&session.dir).unwrap(),
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
            credential_files: Vec::new(),
            rules: session.rules.clone(),
        },
    )
    .unwrap();
    let finished = run(resumed);
    let seen = statuses_until(&mut lines, |_| true);
    assert_eq!(seen[0].payload["name"], "say hi");
    assert_eq!(seen[0].payload["state"], "idle");
    drop(inbox);
    finished
        .recv_timeout(DEADLINE)
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
