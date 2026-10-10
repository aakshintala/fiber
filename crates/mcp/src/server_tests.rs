//! The server against the fixture and a fake clock: every wait carries
//! a named deadline, and the clock advances only after `await_parked`
//! proves the caller is waiting on it.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{CallError, ListedTool, Server, StartError};
use crate::test_support::{Setup, WITHIN};

fn echo_tools() -> Value {
    json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
        "annotations": {"readOnlyHint": true},
    }])
}

#[test]
fn initialize_and_list_succeed() {
    let setup = Setup::with_tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start_expect(Duration::from_secs(5));
    assert_eq!(opened.tools.len(), 1);
    let tool = ListedTool::read(&opened.tools[0]);
    assert_eq!(tool.name, "echo");
    assert_eq!(tool.description, "Echoes.");
    assert_eq!(tool.hints.read_only, Some(true));
    assert_eq!(tool.hints.destructive, None);
    // The start sends `notifications/initialized`: without it the fixture's
    // log would miss the notification.
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(
        log.contains("notifications/initialized"),
        "missing initialized notification: {log}"
    );
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
}

#[test]
fn a_call_round_trips() {
    let setup = Setup::with_tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start_expect(Duration::from_secs(5));
    // Calling code that blocks is a wait too (`docs/testing.md`, "Waits and
    // timeouts"): the call runs on a thread and its result is received
    // with a deadline naming the wait.
    let (answer, opened) = fakes::within("the call to `echo`", WITHIN, move || {
        let answer = opened.server.call(
            "echo",
            &json!({"text": "hi"}),
            Duration::from_secs(30),
            &fakes::CancelToken::new(),
        );
        (answer, opened)
    });
    assert_eq!(
        answer.expect("the call answers"),
        json!({"content": [{"type": "text", "text": "hi"}]})
    );
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(log.contains(r#""method":"tools/call""#));
    assert!(log.contains(r#""name":"echo""#));
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
}

#[test]
fn two_concurrent_calls_resolve_by_id_out_of_order() {
    let tools = json!([{"name": "slow"}, {"name": "fast"}]);
    let setup = Setup::with_tools(&tools);
    setup.result("slow", r#"{"content":[{"type":"text","text":"slow"}]}"#);
    setup.result("fast", r#"{"content":[{"type":"text","text":"fast"}]}"#);
    for name in ["held-slow", "release-slow"] {
        let status = std::process::Command::new("mkfifo")
            .arg(setup.dir.path().join(name))
            .status()
            .expect("mkfifo runs");
        assert!(status.success(), "mkfifo creates {name}");
    }
    let opened = setup.start_expect(Duration::from_secs(5));
    let server = std::sync::Arc::new(opened.server);
    let (done, results) = mpsc::channel();
    {
        let server = std::sync::Arc::clone(&server);
        let done = done.clone();
        thread::spawn(move || {
            let answer = server.call(
                "slow",
                &json!({}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send(("slow".to_owned(), answer)).expect("collected");
        });
    }
    {
        let held_path = setup.dir.path().join("held-slow");
        let (held_done, held) = mpsc::channel();
        thread::spawn(move || {
            let content = std::fs::read_to_string(&held_path).expect("held");
            held_done.send(content).expect("collected");
        });
        let ack = held
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the fake holds the slow call within {WITHIN:?}"));
        assert_eq!(
            ack, "held\n",
            "the fake holds the slow call before the fast one starts",
        );
    }
    {
        let server = std::sync::Arc::clone(&server);
        let done = done.clone();
        thread::spawn(move || {
            let answer = server.call(
                "fast",
                &json!({}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send(("fast".to_owned(), answer)).expect("collected");
        });
    }
    drop(done);
    let first: (String, Result<Value, CallError>) =
        results.recv_timeout(WITHIN).unwrap_or_else(|_| {
            panic!("the fast call answers while the slow one is held within {WITHIN:?}")
        });
    assert_eq!(
        (first.0, first.1.expect("no call fails"),),
        (
            "fast".to_owned(),
            json!({"content": [{"type": "text", "text": "fast"}]}),
        ),
        "the fast call answers while the slow one is held",
    );
    {
        let release_path = setup.dir.path().join("release-slow");
        let (released_done, released) = mpsc::channel();
        thread::spawn(move || {
            std::fs::write(&release_path, "release\n").expect("release");
            released_done.send(()).expect("collected");
        });
        released
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the slow release completes within {WITHIN:?}"));
    }
    let second: (String, Result<Value, CallError>) = results
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the slow call answers once released within {WITHIN:?}"));
    assert_eq!(
        (second.0, second.1.expect("no call fails"),),
        (
            "slow".to_owned(),
            json!({"content": [{"type": "text", "text": "slow"}]}),
        ),
        "the slow call answers once released",
    );
}

#[test]
fn a_hang_tool_times_out_only_after_the_clock_advances() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let answer = opened
            .server
            .call("hang", &json!({}), timeout, &fakes::CancelToken::new());
        done.send(answer).expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the caller waits on the call deadline within {WITHIN:?}",
    );
    let mark = setup.fake.advance_marked(Duration::from_secs(59));
    assert!(
        setup.fake.await_parked_since(&mark, Some(deadline), WITHIN),
        "the call waits again a second before its deadline within {WITHIN:?}"
    );
    assert!(
        result.try_recv().is_err(),
        "the call is still waiting a second before its deadline"
    );
    setup.fake.advance(Duration::from_secs(1));
    assert_eq!(
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the timed-out call answers within {WITHIN:?}")),
        Err(CallError::Timeout),
    );
}

#[test]
fn cancel_ends_the_wait_and_sends_cancelled() {
    let setup = Setup::with_tools(&json!([{"name":"hang"},{"name":"echo"}]));
    setup.result("hang", "hang");
    setup.result("echo", r#"{"content":[]}"#);
    let opened = setup.start_expect(Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    // Detached holding a clone: the server stays alive until the log below
    // was read, so the fixture cannot die under the assertion
    // (`Server::drop` kills it). Detached, not scoped: a caller parked on
    // the fake clock would otherwise keep the scope's implicit join waiting
    // after a failed assertion, so the test would hang instead of failing.
    let server = std::sync::Arc::new(opened.server);
    {
        let server = std::sync::Arc::clone(&server);
        let cancel = cancel.clone();
        thread::spawn(move || {
            let answer = server.call("hang", &json!({}), timeout, &cancel);
            done.send(answer).expect("collected");
        });
    }
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the caller waits on the call deadline within {WITHIN:?}",
    );
    cancel.cancel();
    // The wake proves the bridge: without it the waiter would sit
    // parked until the clock moves. On a miss, move the clock so the
    // parked caller can still answer, then fail naming the wake.
    match result.recv_timeout(WITHIN) {
        Ok(answer) => assert_eq!(answer, Err(CallError::Cancelled)),
        Err(_) => {
            setup.fake.advance(timeout);
            match result.recv_timeout(WITHIN) {
                Ok(_) | Err(_) => {}
            }
            panic!("cancel did not wake the waiter within {WITHIN:?} without a clock move");
        }
    }
    // The fixture reads stdin in order and logs each line before it
    // handles it; the waiter wrote `notifications/cancelled` before it
    // answered, so the answer to a later call proves it is logged.
    let after = std::sync::Arc::clone(&server);
    let answer = fakes::within("a call after the cancel", WITHIN, move || {
        after.call(
            "echo",
            &json!({}),
            Duration::from_secs(30),
            &fakes::CancelToken::new(),
        )
    });
    assert_eq!(answer.expect("echo answers"), json!({"content": []}));
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    let cancelled = log
        .find("notifications/cancelled")
        .unwrap_or_else(|| panic!("no notifications/cancelled: {log}"));
    let echo = log
        .find(r#""name":"echo""#)
        .expect("the echo call is logged");
    assert!(
        cancelled < echo,
        "cancelled is logged before the later call: {log}"
    );
}

#[test]
fn a_non_object_initialize_reply_fails_the_start() {
    // A `result` that is not an object is not a handshake: the start
    // fails naming `initialize`, rather than moving on to `tools/list`.
    // The script runs as `bash -c`, never as a file written and executed
    // here: macOS can hold the first exec of a newly written executable
    // in `_dyld_start` for seconds (seen on #754), which no deadline
    // short of the nextest kill covers.
    let dir = TempDir::new("fiber-mcp-bad-init");
    let script = "IFS= read -r line\nid=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\nprintf '%s\\n' \"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":$id,\\\"result\\\":42}\"\n";
    let workspace = dir.path().to_path_buf();
    let fake = FakeClock::new();
    let clock: std::sync::Arc<dyn Clock> = fake;
    let error = Setup::start_result(
        "/bin/bash",
        &["-c".to_owned(), script.to_owned()],
        &workspace,
        &clock,
        Duration::from_secs(5),
    )
    .err()
    .expect("a non-object initialize fails");
    match error {
        StartError::StartFailed(message) => assert!(
            message.contains("initialize"),
            "unexpected message: {message}"
        ),
        StartError::Deadline => panic!("a fast non-object reply is not a deadline"),
    }
}

#[test]
fn a_command_that_does_not_exist_fails_the_start() {
    let setup = Setup::with_tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let error = Setup::start_result(
        "/no/such/command",
        &[],
        &workspace,
        &setup.clock(),
        Duration::from_secs(5),
    )
    .err()
    .expect("an unknown command fails");
    assert!(matches!(error, StartError::StartFailed(_)));
}

#[test]
fn a_server_that_exits_at_once_fails_the_start() {
    let setup = Setup::with_tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let clock = setup.clock();
    let error = Setup::start_result("/bin/true", &[], &workspace, &clock, Duration::from_secs(5))
        .err()
        .expect("an instant exit fails");
    assert!(matches!(error, StartError::StartFailed(_)));
}

#[test]
fn a_server_that_misses_its_startup_deadline_is_left_out() {
    let setup = Setup::with_tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let timeout = Duration::from_secs(5);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let clock = setup.clock();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        // `sleep` answers nothing: only the deadline ends the start.
        let error = Server::start(
            "/bin/sleep",
            &["30".to_owned()],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        )
        .err()
        .expect("a silent server misses its deadline");
        done.send(error).expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the start waits on the startup deadline within {WITHIN:?}",
    );
    setup.fake.advance(timeout);
    assert_eq!(
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the missed-deadline start ends within {WITHIN:?}")),
        StartError::Deadline,
    );
}

#[test]
fn a_server_killed_mid_call_is_gone() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    let shared = std::sync::Arc::clone(&opened.server.inner.as_ref().expect("running").shared);
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    await_gone(&shared);
    // Gone is set, so the call answers without parking on the clock.
    let answer = fakes::within("a call to a gone server", WITHIN, move || {
        opened.server.call(
            "hang",
            &json!({}),
            Duration::from_secs(1),
            &fakes::CancelToken::new(),
        )
    });
    assert_eq!(answer, Err(CallError::Gone));
}

#[test]
fn garbage_on_stdout_is_ignored() {
    let setup = Setup::with_tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    setup.write( "noise", "not json at all\n[1, 2, 3]\n");
    let opened = setup.start_expect(Duration::from_secs(5));
    let (answer, opened) = fakes::within(
        "the call to `echo` past garbage on stdout",
        WITHIN,
        move || {
            let answer = opened.server.call(
                "echo",
                &json!({"text": "hi"}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            (answer, opened)
        },
    );
    assert_eq!(
        answer.expect("calls work past the garbage"),
        json!({"content": [{"type": "text", "text": "hi"}]})
    );
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
}

#[test]
fn closing_stdin_lets_the_server_exit_on_eof() {
    // The reader holds only a `Weak` sender, so `shutdown` closes stdin
    // and the fixture's `read` loop sees EOF and exits on its own: the
    // next call sees `gone` without any kill. With a strong sender in the
    // reader the channel would stay open and every call would time out.
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let mut opened = setup.start_expect(Duration::from_secs(5));
    let shared = std::sync::Arc::clone(&opened.server.inner.as_ref().expect("running").shared);
    opened.server.shutdown();
    await_gone(&shared);
    // Gone is set, so the call answers without parking on the clock.
    let answer = fakes::within("a call to a gone server", WITHIN, move || {
        opened.server.call(
            "hang",
            &json!({}),
            Duration::from_secs(1),
            &fakes::CancelToken::new(),
        )
    });
    assert_eq!(answer, Err(CallError::Gone));
}

#[test]
fn dropping_the_server_reaps_the_child() {
    // Without `Drop`'s kill and reap the child would stay (as a zombie:
    // `kill -0` still finds it) after the handles are gone.
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    let pid = setup.pid();
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server runs before the drop",
    );
    fakes::within("the drop to kill and reap the server", WITHIN, move || {
        drop(opened.server)
    });
    assert!(
        !fakes::kill_pid(pid, "0").expect("probe"),
        "pid {pid} is still there after the drop"
    );
}

#[test]
fn server_requests_are_answered_ping_ok_and_unknown_32601() {
    let setup = Setup::with_tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.write( "ping-on-start", "");
    let opened = setup.start_expect(Duration::from_secs(5));
    // The fixture prints `ping` and `bogus` before it reads anything. The
    // reader answers each one through the shared writer channel before it
    // reads the `initialize` reply, and `start` returns only after that
    // reply. So once `start` returns, the answers are already logged.
    let (answer, opened) = fakes::within("a call after the server's requests", WITHIN, move || {
        let answer = opened.server.call(
            "echo",
            &json!({}),
            Duration::from_secs(30),
            &fakes::CancelToken::new(),
        );
        (answer, opened)
    });
    assert_eq!(answer.expect("echo answers"), json!({"content": []}));
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(
        log.contains("\"id\":\"probe\""),
        "missing ping probe: {log}"
    );
    assert!(log.contains("\"result\":{}"), "missing ping result: {log}");
    assert!(
        log.contains("\"id\":\"bogus\""),
        "missing bogus probe: {log}"
    );
    assert!(
        log.contains("-32601"),
        "missing unknown-method error: {log}"
    );
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
}

#[test]
fn stop_leaves_no_running_child() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    let pid = setup.pid();
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server runs before the stop",
    );
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
    assert!(
        !fakes::kill_pid(pid, "0").expect("probe"),
        "pid {pid} is still there after the stop"
    );
}

/// A server that keeps running after its stdin ends: the fixture under a
/// shell that loops once it returns. `term` is the shell's TERM trap:
/// `exit 0` to end on SIGTERM, empty to ignore it.
fn lingering(setup: &Setup, term: &str) -> super::OpenServer {
    let script = format!("trap '{term}' TERM\n\"$1\" \"$2\"\nwhile :; do sleep 0.05; done\n");
    let fixture = fakes::mcp_fixture().display().to_string();
    let workspace = setup.dir.path().to_path_buf();
    let clock = setup.clock();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let outcome = Server::start(
            "/bin/bash",
            &[
                "-c".to_owned(),
                script,
                "lingering".to_owned(),
                fixture,
                workspace.display().to_string(),
            ],
            &BTreeMap::new(),
            &workspace,
            &clock,
            Duration::from_secs(5),
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the lingering server starts within {WITHIN:?}"))
        .expect("the lingering server starts")
}

/// Runs `stop` on its own thread; the receiver hears when it returns.
fn stopping(server: super::Server) -> mpsc::Receiver<()> {
    let (done, stopped) = mpsc::channel();
    thread::spawn(move || {
        server.stop();
        done.send(()).expect("collected");
    });
    stopped
}

#[test]
fn a_server_that_ignores_end_of_input_stops_on_sigterm_before_the_grace() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    let opened = lingering(&setup, "exit 0");
    let stopped = stopping(opened.server);
    // The clock never moves: only the SIGTERM ends the server.
    stopped.recv_timeout(WITHIN).unwrap_or_else(|_| {
        panic!("the stop returned without the grace passing within {WITHIN:?}")
    });
}

#[test]
fn kill_every_server_kills_one_that_ignores_sigterm() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    let opened = lingering(&setup, "");
    let grace = setup.fake.now() + super::GRACE;
    let stopped = stopping(opened.server);
    assert!(
        setup.fake.await_parked(grace, WITHIN),
        "the stop waits out the grace on a server ignoring SIGTERM"
    );
    crate::registry::kill_every_server();
    // Killed, its output ends: the stop returns with the clock unmoved.
    stopped.recv_timeout(WITHIN).unwrap_or_else(|_| {
        panic!("the stop returned once the server was killed within {WITHIN:?}")
    });
}

#[test]
fn a_server_is_listed_until_its_reap() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    assert_eq!(super::lock(&super::LIVE).len(), 1);
    stopping(opened.server)
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop returned within {WITHIN:?}"));
    assert!(super::lock(&super::LIVE).is_empty());
}

/// What [`super::before_signal`] runs in this test process, if anything.
static BEFORE_SIGNAL: std::sync::Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> =
    std::sync::Mutex::new(None);

pub(crate) fn before_signal() {
    let hook = super::lock(&BEFORE_SIGNAL).clone();
    if let Some(hook) = hook {
        hook();
    }
}

/// Where [`super::before_lock`] reports, if anywhere.
static BEFORE_LOCK: std::sync::Mutex<Option<mpsc::Sender<&'static str>>> =
    std::sync::Mutex::new(None);

pub(crate) fn before_lock(which: &'static str) {
    if let Some(tx) = super::lock(&BEFORE_LOCK).as_ref() {
        tx.send(which).unwrap_or(());
    }
}

/// Reports each lock a reap is about to take.
fn watch_reap_locks() -> mpsc::Receiver<&'static str> {
    let (tx, rx) = mpsc::channel();
    *super::lock(&BEFORE_LOCK) = Some(tx);
    rx
}

/// Waits, under one [`WITHIN`], until a reap reports it is about to take
/// `which`.
fn await_lock(locks: mpsc::Receiver<&'static str>, which: &'static str) {
    fakes::within("the reap to reach its lock", WITHIN, move || {
        loop {
            let reached = locks
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("the reap reached the {which} lock within {WITHIN:?}"));
            if reached == which {
                return;
            }
        }
    });
}

/// Pauses the next signaller at [`super::before_signal`]: `entered` hears
/// it arrive, and it goes on once `go` is sent.
fn pause_signallers() -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (go, go_rx) = mpsc::channel::<()>();
    let entered_tx = std::sync::Mutex::new(entered_tx);
    let go_rx = std::sync::Mutex::new(go_rx);
    *super::lock(&BEFORE_SIGNAL) = Some(std::sync::Arc::new(move || {
        super::lock(&entered_tx)
            .send(())
            .expect("the test is waiting");
        super::lock(&go_rx)
            .recv()
            .unwrap_or_else(|_| panic!("the test let the signaller go"));
    }));
    (entered, go)
}

/// Waits, at most [`WITHIN`], until the server's output has ended.
fn await_gone(shared: &super::Shared) {
    let state = super::lock(&shared.inner);
    let (state, _) = shared
        .cv
        .wait_timeout_while(state, WITHIN, |state| !state.gone)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(state.gone, "the server's output ended within {WITHIN:?}");
}

#[test]
fn kill_every_server_holds_its_pids_unreaped_while_it_signals() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start_expect(Duration::from_secs(5));
    let pid = setup.pid();
    let shared = std::sync::Arc::clone(&opened.server.inner.as_ref().expect("running").shared);
    let (entered, go) = pause_signallers();
    let (killed_tx, killed) = mpsc::channel();
    thread::spawn(move || {
        crate::registry::kill_every_server();
        killed_tx.send(()).expect("collected");
    });
    entered
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("kill_every_server reached its signal within {WITHIN:?}"));
    // A reap races the paused signaller: it stops the server, but cannot
    // reap it while the signaller holds the list.
    let locks = watch_reap_locks();
    let (reaped_tx, reaped) = mpsc::channel();
    thread::spawn(move || {
        drop(opened.server);
        reaped_tx.send(()).expect("collected");
    });
    await_lock(locks, "live");
    await_gone(&shared);
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the pid was reaped while kill_every_server held it"
    );
    assert!(
        reaped.try_recv().is_err(),
        "the reap finished under the signaller"
    );
    go.send(()).expect("the signaller waits");
    killed
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("kill_every_server returned within {WITHIN:?}"));
    reaped
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the reap finished within {WITHIN:?}"));
    assert!(!super::lock(&super::LIVE).contains(&pid));
}

#[test]
fn stop_holds_the_child_unreaped_while_it_sends_sigterm() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    let opened = lingering(&setup, "exit 0");
    let server = std::sync::Arc::new(opened.server);
    let shared = std::sync::Arc::clone(&server.inner.as_ref().expect("running").shared);
    let pid = super::lock(&server.inner.as_ref().expect("running").child)
        .as_ref()
        .expect("unreaped")
        .id();
    let (entered, go) = pause_signallers();
    let stopping = std::sync::Arc::clone(&server);
    let (stopped_tx, stopped) = mpsc::channel();
    thread::spawn(move || {
        stopping.stop();
        stopped_tx.send(()).expect("collected");
    });
    entered
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("stop reached its SIGTERM within {WITHIN:?}"));
    // A reap races the paused SIGTERM: it cannot take the child while stop
    // holds it, so the server is neither killed nor reaped yet.
    let locks = watch_reap_locks();
    let reaping = std::sync::Arc::clone(&server);
    let (reaped_tx, reaped) = mpsc::channel();
    thread::spawn(move || {
        reaping.reap();
        reaped_tx.send(()).expect("collected");
    });
    await_lock(locks, "child");
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server was reaped while stop held it"
    );
    assert!(
        reaped.try_recv().is_err(),
        "the reap finished under the SIGTERM"
    );
    assert!(!super::lock(&shared.inner).gone, "the server still runs");
    go.send(()).expect("stop waits");
    // The SIGTERM ends the server: the clock never moves.
    stopped
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("stop returned within {WITHIN:?}"));
    reaped
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the reap finished within {WITHIN:?}"));
    assert!(!super::lock(&super::LIVE).contains(&pid));
}

/// A server start on its own thread; the receiver hears the outcome.
fn starting(
    command: &str,
    args: &[String],
    clock: &std::sync::Arc<dyn Clock>,
    workspace: &std::path::Path,
    timeout: Duration,
) -> mpsc::Receiver<Result<super::OpenServer, StartError>> {
    let command = command.to_owned();
    let args = args.to_owned();
    let clock = std::sync::Arc::clone(clock);
    let workspace = workspace.to_path_buf();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let outcome = Server::start(
            &command,
            &args,
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    result
}

/// A server that never answers and ignores SIGTERM: `trap '' TERM` is
/// inherited across `exec`, so `sleep` holds its stdout pipe open until it
/// is killed. The script writes its pid to `ready` after installing the
/// trap, so the wait below proves the trap is set before the stop signals
/// it.
fn starting_silent_ignoring(
    ready: &std::path::Path,
    clock: &std::sync::Arc<dyn Clock>,
    workspace: &std::path::Path,
    timeout: Duration,
) -> mpsc::Receiver<Result<super::OpenServer, StartError>> {
    let quoted = ready.display().to_string().replace('\'', "'\\''");
    let script = format!("trap '' TERM\necho $$ > '{quoted}'\nexec sleep 300");
    starting(
        "/bin/bash",
        &["-c".to_owned(), script],
        clock,
        workspace,
        timeout,
    )
}

fn assert_shutdown_failed(outcome: Result<super::OpenServer, StartError>) {
    match outcome {
        Err(StartError::StartFailed(message)) => assert_eq!(
            message, "Fiber is shutting down.",
            "unexpected message: {message}"
        ),
        Err(StartError::Deadline) => panic!("a stopped start is not a deadline"),
        Ok(_) => panic!("a stopped start does not open"),
    }
}

#[test]
fn a_stopped_start_waits_out_the_grace_before_its_kill() {
    // Without the grace wait the stop would kill at once, as a plain
    // shutdown and reap does: the probe below pins the wait.
    let setup = Setup::with_tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let ready = fakes::children::Ready::new(setup.dir.path());
    let result = starting_silent_ignoring(
        ready.path(),
        &setup.clock(),
        &workspace,
        Duration::from_secs(600),
    );
    // After the trap: the script writes its pid only once `trap '' TERM`
    // is set, so the stop below cannot signal before it ignores SIGTERM.
    ready.wait(WITHIN);
    crate::registry::stop_every_start();
    let grace = setup.fake.now().checked_add(super::GRACE).expect("grace");
    assert!(
        setup.fake.await_parked(grace, WITHIN),
        "the stop waits out the grace within {WITHIN:?}"
    );
    let pid = super::lock(&super::LIVE).first().copied().expect("listed");
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server was killed before the grace passed"
    );
    setup.fake.advance(super::GRACE);
    assert_shutdown_failed(
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends once the grace passes within {WITHIN:?}")),
    );
    assert!(
        !fakes::kill_pid(pid, "0").expect("probe"),
        "the server was reaped after the grace"
    );
    assert!(super::lock(&super::LIVE).is_empty());
}

#[test]
fn a_stopped_start_whose_server_exits_on_sigterm_returns_without_the_clock_moving() {
    let setup = Setup::with_tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    // Silent, but SIGTERM ends it: its output ends inside the grace.
    let start_deadline = setup
        .fake
        .now()
        .checked_add(Duration::from_secs(600))
        .expect("deadline");
    let result = starting(
        "/bin/sleep",
        &["30".to_owned()],
        &setup.clock(),
        &workspace,
        Duration::from_secs(600),
    );
    assert!(
        setup.fake.await_parked(start_deadline, WITHIN),
        "the start waits on its startup deadline within {WITHIN:?}"
    );
    assert_eq!(
        super::lock(&super::LIVE).len(),
        1,
        "the start listed its child before it waited"
    );
    let before = setup.fake.now();
    crate::registry::stop_every_start();
    assert_shutdown_failed(
        result.recv_timeout(WITHIN).unwrap_or_else(|_| {
            panic!("the start ends without the clock moving within {WITHIN:?}")
        }),
    );
    assert_eq!(setup.fake.now(), before, "the clock never moved");
    assert!(super::lock(&super::LIVE).is_empty());
}

#[test]
fn a_start_after_stop_every_start_spawns_nothing() {
    let setup = Setup::with_tools(&json!([]));
    crate::registry::stop_every_start();
    let workspace = setup.dir.path().to_path_buf();
    let result = starting(
        "/bin/true",
        &[],
        &setup.clock(),
        &workspace,
        Duration::from_secs(5),
    );
    assert_shutdown_failed(
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start returns at once within {WITHIN:?}")),
    );
    assert!(
        super::lock(&super::LIVE).is_empty(),
        "nothing spawned after the stop"
    );
}

#[test]
fn is_gone_turns_true_once_the_server_exits() {
    let setup = Setup::with_tools(&json!([{"name": "hang"}]));
    let opened = setup.start_expect(Duration::from_secs(5));
    assert!(!opened.server.is_gone(), "a running server is not gone");
    let shared = std::sync::Arc::clone(&opened.server.inner.as_ref().expect("running").shared);
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    await_gone(&shared);
    assert!(opened.server.is_gone());
}

#[test]
fn a_server_with_no_connection_is_gone() {
    assert!(Server { inner: None }.is_gone());
}
