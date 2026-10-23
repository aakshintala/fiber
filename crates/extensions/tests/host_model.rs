//! `host.model` through the public API (`docs/extensions.md`, "Host calls"):
//! a command, a hook and a timer call the session's model through the
//! injected [`HostModel`], and every attempt's line reaches the inbox.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use common::{Setup, install_lua};
use config::{Config, ProjectKey, Sources};
use contract::clock::Clock as _;
use contract::events::{CallStatus, Event, UsageRecorded};
use contract::extension::ExtensionDoor;
use contract::hook::{AfterToolCall, BeforeModelCall, Hooks};
use contract::inbox::Delivery;
use contract::model::{Bound, HostModel, HostModelReply, HostModelRequest};
use contract::shapes::{Failure, Tokens};
use contract::{CommandId, ErrorCode, GenerationId};
use extensions::SessionExtensions;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Deadline, MUST_SUCCEED_WITHIN, NoLock};
use serde_json::{Map, json};

/// A recorded `host.model` call: who asked and what for.
#[derive(Debug)]
struct Seen {
    extension: String,
    model: String,
    system: Option<String>,
    messages: usize,
    max_tokens: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fake [`HostModel`] answering every call at once with `text`.
struct Answer {
    text: &'static str,
    seen: Mutex<Vec<Seen>>,
}

impl Answer {
    fn new(text: &'static str) -> Self {
        Self {
            text,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl HostModel for Answer {
    fn call(
        &self,
        extension: &str,
        request: HostModelRequest,
        _bound: Bound,
    ) -> Result<HostModelReply, Failure> {
        lock(&self.seen).push(Seen {
            extension: extension.to_owned(),
            model: request.model,
            system: request.system,
            messages: request.messages.len(),
            max_tokens: request.max_tokens,
        });
        Ok(HostModelReply {
            text: self.text.to_owned(),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 2,
            },
            cost: None,
        })
    }
}

/// A fake [`HostModel`] blocking until the test releases it, then passing
/// `line` to `bound.lines` and answering `text`.
struct Block {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    line: UsageRecorded,
    text: &'static str,
}

impl HostModel for Block {
    #[allow(clippy::panic, reason = "the test fails if the fake is never released")]
    fn call(
        &self,
        _extension: &str,
        _request: HostModelRequest,
        bound: Bound,
    ) -> Result<HostModelReply, Failure> {
        let _sent = self.entered.send(());
        lock(&self.release)
            .recv()
            .unwrap_or_else(|_| panic!("the test releases the blocked call"));
        (bound.lines)(self.line.clone());
        Ok(HostModelReply {
            text: self.text.to_owned(),
            tokens: Tokens {
                input: 1,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 1,
            },
            cost: None,
        })
    }
}

/// A fake [`HostModel`] passing each of `lines` to `bound.lines`, in
/// order, then answering `ok`.
struct Lines {
    lines: Vec<UsageRecorded>,
    seen: Mutex<Vec<String>>,
}

impl HostModel for Lines {
    fn call(
        &self,
        extension: &str,
        _request: HostModelRequest,
        bound: Bound,
    ) -> Result<HostModelReply, Failure> {
        lock(&self.seen).push(extension.to_owned());
        for line in &self.lines {
            (bound.lines)(line.clone());
        }
        Ok(HostModelReply {
            text: "ok".to_owned(),
            tokens: Tokens {
                input: 1,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 1,
            },
            cost: None,
        })
    }
}

fn line(model: &str, extension: Option<&str>) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(format!("g_{model}")),
        model: model.to_owned(),
        tokens: Tokens {
            input: 1,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 1,
        },
        input_bytes: 7,
        input_media: None,
        web_searches: None,
        cost: None,
        subscription: None,
        extension: extension.map(str::to_owned),
        origin_session_id: None,
        reviewer: None,
    }
}

#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn load(setup: &Setup, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .expect("the test home loads");
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Arc::new(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("waited for the extensions to load"),
    )
}

/// Runs the extension command `name` through the session door: the waiter
/// thread drops the answer, so what the callback did is observed on the
/// inbox.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[track_caller]
fn run(session: &Arc<SessionExtensions>, name: &str) {
    let release = session
        .command(&CommandId("c_test".to_owned()), name, "")
        .expect("the command is admitted");
    release();
}

/// The next `extension_log` line on `inbox`.
#[allow(clippy::panic, reason = "a test helper; a missing line is the test's")]
#[track_caller]
fn next_log(inbox: &mpsc::Receiver<Delivery>, what: &str) -> String {
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(inbox) {
        Ok(Delivery::ExtensionLog(log)) => log.message,
        Ok(other) => panic!("expected {what}, got {other:?}"),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for {what}"),
    }
}

/// The next `extension_usage` line on `inbox`.
#[allow(clippy::panic, reason = "a test helper; a missing line is the test's")]
#[track_caller]
fn next_usage(inbox: &mpsc::Receiver<Delivery>, what: &str) -> UsageRecorded {
    match Deadline::after(MUST_SUCCEED_WITHIN).recv(inbox) {
        Ok(Delivery::ExtensionUsage(line)) => line,
        Ok(other) => panic!("expected {what}, got {other:?}"),
        Err(_) => panic!("waited {MUST_SUCCEED_WITHIN:?} for {what}"),
    }
}

const GO_INIT: &str = r#"
fiber.command("go", { timeout = 1000, run = function()
  local r = host.model({ model = "scripted/e.json", system = "terse",
    messages = { { role = "user", content = "hi" } }, max_tokens = 16 })
  host.log("result:" .. r.text .. ":" .. tostring(r.usage.tokens.input)
    .. ":" .. tostring(r.usage.tokens.output) .. ":" .. tostring(r.usage.cost))
end })
"#;

#[test]
fn a_command_calls_host_model_and_gets_text_and_usage() {
    let setup = Setup::new();
    install_lua(&setup, "m", &json!({}), GO_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Answer::new("ext-ok"));
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go");
    assert_eq!(
        next_log(&inbox_rx, "the model result"),
        "result:ext-ok:10:2:nil"
    );
    let seen = lock(&fake.seen);
    assert_eq!(seen.len(), 1, "one call reached the fake: {seen:?}");
    assert_eq!(seen[0].model, "scripted/e.json");
    assert_eq!(seen[0].system.as_deref(), Some("terse"));
    assert_eq!(seen[0].messages, 1);
    assert_eq!(seen[0].max_tokens, 16);
}

#[test]
fn the_fake_sees_the_calling_extensions_name() {
    let setup = Setup::new();
    install_lua(&setup, "m", &json!({}), GO_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Answer::new("ok"));
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go");
    assert_eq!(
        next_log(&inbox_rx, "the model result"),
        "result:ok:10:2:nil"
    );
    assert_eq!(lock(&fake.seen)[0].extension, "fiber.test/m");
}

#[test]
fn a_hook_and_a_timer_may_call_host_model() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "m",
        &json!({}),
        r#"
fiber.hook("after_tool", { timeout = 1000, on_failure = "blocking", run = function(call)
  local r = host.model({ model = "p/m",
    messages = { { role = "user", content = "hi" } }, max_tokens = 8 })
  host.log("hook:" .. r.text)
  return { content = call.content }
end })
host.after(50, function()
  local r = host.model({ model = "p/m",
    messages = { { role = "user", content = "hi" } }, max_tokens = 8 })
  host.log("timer:" .. r.text)
end, { timeout = 1000 })
"#,
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock.clone());
    let fake = Arc::new(Answer::new("ext-ok"));
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    let arguments = Map::new();
    let answer = session.after_tool(&AfterToolCall {
        tool: "read",
        arguments: &arguments,
        status: CallStatus::Completed,
        content: "x",
        details: None,
        process: None,
        spilled: None,

        not_regular: false,
        delivery: None,
    });
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    assert_eq!(
        next_log(&inbox_rx, "the hook's model result"),
        "hook:ext-ok"
    );
    // The scheduler parks at the timer's due before the clock moves: the
    // advance then fires the timer, whose callback calls `host.model` once.
    let due = clock.now() + Duration::from_millis(50);
    let _mark = clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(50),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(
        next_log(&inbox_rx, "the timer's model result"),
        "timer:ext-ok"
    );
    assert_eq!(lock(&fake.seen).len(), 2, "the hook and the timer called");
}

const TIMEOUT_INIT: &str = r#"
fiber.command("go", { timeout = 50, run = function()
  local r = host.model({ model = "p/m",
    messages = { { role = "user", content = "hi" } }, max_tokens = 16 })
  host.log("got:" .. r.text)
end })
"#;

#[test]
fn a_timed_out_callback_drops_the_late_reply_but_keeps_the_lines() {
    let setup = Setup::new();
    install_lua(&setup, "m", &json!({}), TIMEOUT_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock.clone());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let fake = Arc::new(Block {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        line: line("p/m", Some("fiber.test/m")),
        text: "LATE",
    });
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    let (log, emitted) = fakes::EventLog::channel();
    session.emit_to(Arc::new(log) as Arc<dyn contract::emit::Emit>);
    run(&session, "go");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&entered_rx)
        .expect("waited for the fake to enter the call");
    // The fake is inside `call` and the callback is parked: the
    // scheduler waits at the callback's deadline while the waiter sits
    // at deadline plus grace (`GRACE` in `hub.rs`, one second), so the
    // advance names the scheduler's thread. Seeing the waiter at its
    // grace park proves it left the deadline, leaving the scheduler
    // alone at it; the advance then ends the callback `timeout` at its
    // deadline, and the door names the command in its notice.
    let deadline = clock.now() + Duration::from_millis(50);
    let grace = deadline + Duration::from_secs(1);
    assert!(
        clock
            .mark_parked(grace, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
            .is_some(),
        "waited {MUST_SUCCEED_WITHIN:?} for the waiter to park at deadline plus grace",
    );
    let scheduler = clock
        .mark_parked(deadline, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .expect("waited for the scheduler to park at the callback deadline")
        .threads();
    assert_eq!(
        scheduler.len(),
        1,
        "only the scheduler waits at the deadline: {scheduler:?}"
    );
    let _mark = clock.advance_when_parked(
        Some(deadline),
        Some(scheduler[0]),
        Duration::from_millis(50),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let noticed = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&emitted)
        .expect("waited for the timeout notice");
    let Event::Notice(notice) = noticed else {
        panic!("expected the timeout notice, got {noticed:?}");
    };
    assert_eq!(notice.code, ErrorCode::ExtensionFailed);
    assert!(
        notice.message.contains("go"),
        "the timeout names the command: {}",
        notice.message
    );
    // The spend is real: the line the fake pushes after the timeout is
    // still delivered, while the late reply resumes nothing.
    let _sent = release_tx.send(());
    let spent = next_usage(&inbox_rx, "the timed-out attempt's line");
    assert_eq!(spent.model, "p/m");
    assert_eq!(spent.extension.as_deref(), Some("fiber.test/m"));
    assert!(
        matches!(inbox_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the late reply resumed no coroutine"
    );
}

#[test]
fn before_model_call_never_runs_for_the_call() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "m",
        &json!({}),
        r#"
fiber.hook("before_model_call", { timeout = 1000, on_failure = "blocking", run = function(call)
  host.log("bmc")
  return {}
end })
fiber.command("go", { timeout = 1000, run = function()
  local r = host.model({ model = "p/m",
    messages = { { role = "user", content = "hi" } }, max_tokens = 8 })
  host.log("result:" .. r.text)
end })
"#,
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Answer::new("ext-ok"));
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go");
    assert_eq!(next_log(&inbox_rx, "the model result"), "result:ext-ok");
    assert!(
        matches!(inbox_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "before_model_call ran for the extension's own call"
    );
    // The hook itself works: asked directly, it runs once.
    let usage = contract::shapes::Usage::default();
    let cancel = CancelToken::new();
    let answer = session.before_model_call(&BeforeModelCall {
        model: "p/m",
        usage: &usage,
        cancel: &cancel,
    });
    assert!(answer.notices.is_empty(), "{:?}", answer.notices);
    assert_eq!(next_log(&inbox_rx, "the hook's own run"), "bmc");
}

#[test]
fn a_line_with_an_explicit_extension_reaches_the_inbox() {
    let setup = Setup::new();
    install_lua(&setup, "m", &json!({}), GO_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Lines {
        lines: vec![line("p/m", Some("tag"))],
        seen: Mutex::new(Vec::new()),
    });
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go");
    let spent = next_usage(&inbox_rx, "the tagged line");
    assert_eq!(spent.extension.as_deref(), Some("tag"));
    assert_eq!(next_log(&inbox_rx, "the model result"), "result:ok:1:1:nil");
}

#[test]
fn failed_attempt_lines_arrive_in_order_before_the_success() {
    let setup = Setup::new();
    install_lua(&setup, "m", &json!({}), GO_INIT);
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let fake = Arc::new(Lines {
        lines: vec![
            line("p/try1", Some("fiber.test/m")),
            line("p/try2", Some("fiber.test/m")),
            line("p/final", Some("fiber.test/m")),
        ],
        seen: Mutex::new(Vec::new()),
    });
    session.model_to(Arc::clone(&fake) as Arc<dyn HostModel>);
    let (inbox_tx, inbox_rx) = mpsc::channel();
    session.deliver_to(inbox_tx);
    run(&session, "go");
    let models: Vec<String> = (0..3)
        .map(|_| next_usage(&inbox_rx, "an attempt line").model.clone())
        .collect();
    assert_eq!(models, vec!["p/try1", "p/try2", "p/final"]);
    assert_eq!(*lock(&fake.seen), vec!["fiber.test/m".to_owned()]);
    assert_eq!(next_log(&inbox_rx, "the model result"), "result:ok:1:1:nil");
}
