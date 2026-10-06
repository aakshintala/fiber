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

use super::{CallError, Server, StartError};

/// How long a test waits for a thread or a child, in real time.
///
/// The largest round value that keeps every test's serial deadlines within
/// half of nextest's 120 s kill: the worst test,
/// `cancel_ends_the_wait_and_sends_cancelled`, can exhaust five
/// (5 x 10 s = 50 s <= 60 s). A passing run never waits on it; it only
/// bounds a hang.
const WITHIN: Duration = Duration::from_secs(10);

/// One real-time poll of a child's exit.
const POLL: Duration = Duration::from_millis(50);

/// Poll iterations that span one `WITHIN` of `POLL` sleeps.
const POLLS: u128 = WITHIN.as_millis() / POLL.as_millis();

struct Setup {
    dir: TempDir,
    fake: std::sync::Arc<FakeClock>,
}

impl Setup {
    fn tools(tools: &Value) -> Self {
        let dir = TempDir::new("fiber-mcp-server");
        write(&dir, "tools.json", &tools.to_string());
        Self {
            dir,
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> std::sync::Arc<dyn Clock> {
        self.fake.clone()
    }

    fn result(&self, tool: &str, body: &str) {
        write(&self.dir, &format!("call-{tool}.json"), body);
    }

    fn start(&self, timeout: Duration) -> super::OpenServer {
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = self.dir.path().to_path_buf();
        let arg = workspace.display().to_string();
        Self::start_result(&script, &[arg], &workspace, &self.clock(), timeout)
            .expect("the fixture server starts")
    }

    fn start_result(
        command: &str,
        args: &[String],
        workspace: &std::path::Path,
        clock: &std::sync::Arc<dyn Clock>,
        timeout: Duration,
    ) -> Result<super::OpenServer, StartError> {
        // Threaded with a wall-clock limit: without `send`, `insert`,
        // `deliver` or `read_stdout` the handshake would sit parked on the
        // fake clock forever, so a bare direct start would hang the test
        // instead of failing it.
        let command = command.to_owned();
        let args = args.to_owned();
        let workspace = workspace.to_path_buf();
        let clock = std::sync::Arc::clone(clock);
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
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }
}

/// One call against a server that may already be gone: when the call is
/// seen parked on its deadline the clock advances past it, and when the
/// call answers without parking nothing moves. Either way the answer is
/// collected; when neither happens within `WITHIN` the test fails naming
/// the wait.
fn gone_call(fake: &FakeClock, server: &Server, timeout: Duration) -> Result<Value, CallError> {
    let deadline = fake.now().checked_add(timeout).expect("deadline");
    let (done, result) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let call = server.call("hang", &json!({}), timeout, &fakes::CancelToken::new());
            done.send(call).expect("collected");
        });
        // Either the answer arrives without a park (the server was gone
        // before the call) or the call is parked on its deadline (then,
        // and only then, the clock moves, once). Polling bounds neither: a
        // late answer or park is seen on a later pass, and only a full
        // `WITHIN` of neither fails the test naming the wait.
        let mut advanced = false;
        for _ in 0..POLLS {
            if !advanced && fake.parked().contains(&Some(deadline)) {
                fake.advance(timeout);
                advanced = true;
            }
            if let Ok(answer) = result.recv_timeout(POLL) {
                return answer;
            }
        }
        // Release a caller parked on the fake clock so the scoped join can
        // still finish, then fail naming the wait (as the cancel test does
        // on its miss path).
        fake.advance(timeout);
        panic!("the call ends within {WITHIN:?}");
    })
}

fn write(dir: &TempDir, name: &str, content: &str) {
    let path = dir.path().join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture parent");
    }
    std::fs::write(path, content).expect("fixture file");
}

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
fn max_line_is_4_mib() {
    assert_eq!(super::MAX_LINE, 4 * 1024 * 1024);
}

#[test]
fn initialize_and_list_succeed() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start(Duration::from_secs(5));
    assert_eq!(opened.tools.len(), 1);
    let tool = &opened.tools[0];
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
    opened.server.stop();
}

#[test]
fn a_call_round_trips() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start(Duration::from_secs(5));
    // Threaded with a wall-clock limit: without `send` the call would sit
    // parked forever, so a bare direct call would hang the test instead of
    // failing it.
    let (done, result) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let answer = opened.server.call(
                "echo",
                &json!({"text": "hi"}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send(answer).expect("collected");
        });
        let answer = result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the call answers within {WITHIN:?}"));
        assert_eq!(
            answer.expect("the call answers"),
            json!({"content": [{"type": "text", "text": "hi"}]})
        );
    });
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(log.contains(r#""method":"tools/call""#));
    assert!(log.contains(r#""name":"echo""#));
    opened.server.stop();
}

#[test]
fn two_concurrent_calls_resolve_by_id_out_of_order() {
    let tools = json!([{"name": "slow"}, {"name": "fast"}]);
    let setup = Setup::tools(&tools);
    setup.result("slow", r#"{"content":[{"type":"text","text":"slow"}]}"#);
    setup.result("fast", r#"{"content":[{"type":"text","text":"fast"}]}"#);
    write(&setup.dir, "delay-slow", "1");
    let opened = setup.start(Duration::from_secs(5));
    let server = std::sync::Arc::new(opened.server);
    let (done, results) = mpsc::channel();
    for tool in ["slow", "fast"] {
        let server = std::sync::Arc::clone(&server);
        let done = done.clone();
        thread::spawn(move || {
            let answer = server.call(
                tool,
                &json!({}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send((tool.to_owned(), answer)).expect("collected");
        });
    }
    drop(done);
    let mut seen = Vec::new();
    for _ in 0..2 {
        let (tool, answer): (String, Result<Value, CallError>) = results
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("both calls answer within {WITHIN:?}"));
        seen.push((tool, answer.expect("no call fails")));
    }
    seen.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        seen,
        [
            (
                "fast".to_owned(),
                json!({"content": [{"type": "text", "text": "fast"}]}),
            ),
            (
                "slow".to_owned(),
                json!({"content": [{"type": "text", "text": "slow"}]}),
            ),
        ],
    );
}

#[test]
fn a_hang_tool_times_out_only_after_the_clock_advances() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
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
    setup.fake.advance(Duration::from_secs(59));
    assert!(
        result.recv_timeout(Duration::from_millis(100)).is_err(),
        "the call is still waiting a second before its deadline",
    );
    setup.fake.advance(Duration::from_secs(1));
    assert_eq!(
        result.recv_timeout(WITHIN).expect("the call ends"),
        Err(CallError::Timeout),
    );
}

#[test]
fn cancel_ends_the_wait_and_sends_cancelled() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    // Scoped: the server stays alive until the log below was read, so the
    // fixture cannot die under the assertion (`Server::drop` kills it).
    thread::scope(|scope| {
        scope.spawn(|| {
            let answer = opened.server.call("hang", &json!({}), timeout, &cancel);
            done.send(answer).expect("collected");
        });
        assert!(
            setup.fake.await_parked(deadline, WITHIN),
            "the caller waits on the call deadline within {WITHIN:?}",
        );
        cancel.cancel();
        // The wake proves the bridge: without it the waiter would sit
        // parked until the clock moves. On a miss, move the clock so the
        // scoped join can still finish, then fail naming the wake.
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
        // The waiter sends `notifications/cancelled` before it answers,
        // but the fixture appends it when it reads it: poll the log.
        let (_held, tick) = mpsc::channel::<()>();
        for _ in 0..POLLS {
            let log =
                std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
            if log.contains("notifications/cancelled") {
                return;
            }
            match tick.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for notifications/cancelled in requests.log");
    });
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
    let setup = Setup::tools(&json!([]));
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
    let setup = Setup::tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let clock = setup.clock();
    let error = Setup::start_result("/bin/true", &[], &workspace, &clock, Duration::from_secs(5))
        .err()
        .expect("an instant exit fails");
    assert!(matches!(error, StartError::StartFailed(_)));
}

#[test]
fn a_server_that_misses_its_startup_deadline_is_left_out() {
    let setup = Setup::tools(&json!([]));
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
        result.recv_timeout(WITHIN).expect("the start ends"),
        StartError::Deadline,
    );
}

#[test]
fn a_server_killed_mid_call_is_gone() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    // The reader marks the server gone when EOF arrives, which races the
    // kill: retry short calls until one sees it, bounding the retries.
    // A call made after `gone` is already set answers at once without
    // parking, so the clock moves only when `await_parked` proves the
    // caller is waiting on it (`docs/testing.md`, "Waits and timeouts").
    let mut answer = Err(CallError::Timeout);
    for _ in 0..50 {
        answer = gone_call(&setup.fake, &opened.server, Duration::from_secs(1));
        if answer == Err(CallError::Gone) {
            break;
        }
    }
    assert_eq!(answer, Err(CallError::Gone));
}

#[test]
fn garbage_on_stdout_is_ignored() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    write(&setup.dir, "noise", "not json at all\n[1, 2, 3]\n");
    let opened = setup.start(Duration::from_secs(5));
    let (done, result) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let answer = opened.server.call(
                "echo",
                &json!({"text": "hi"}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send(answer).expect("collected");
        });
        let answer = result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("calls answer within {WITHIN:?}"));
        assert_eq!(
            answer.expect("calls work past the garbage"),
            json!({"content": [{"type": "text", "text": "hi"}]})
        );
    });
    opened.server.stop();
}

#[test]
fn closing_stdin_lets_the_server_exit_on_eof() {
    // The reader holds only a `Weak` sender, so `shutdown` closes stdin
    // and the fixture's `read` loop sees EOF and exits on its own: the
    // next call sees `gone` without any kill. With a strong sender in the
    // reader the channel would stay open and every call would time out.
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let mut opened = setup.start(Duration::from_secs(5));
    opened.server.shutdown();
    // As above, `gone` may already be set before a retry's call starts:
    // the clock moves only after `await_parked` proves the wait.
    let mut answer = Err(CallError::Timeout);
    for _ in 0..50 {
        answer = gone_call(&setup.fake, &opened.server, Duration::from_secs(1));
        if answer == Err(CallError::Gone) {
            break;
        }
    }
    assert_eq!(answer, Err(CallError::Gone));
}

#[test]
fn dropping_the_server_reaps_the_child() {
    // Without `Drop`'s kill and reap the child would stay (as a zombie:
    // `kill -0` still finds it) after the handles are gone.
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let pid = setup.pid();
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server runs before the drop",
    );
    drop(opened.server);
    let (_held, probe) = mpsc::channel::<()>();
    for _ in 0..POLLS {
        if !fakes::kill_pid(pid, "0").expect("probe") {
            return;
        }
        match probe.recv_timeout(POLL) {
            Ok(()) | Err(_) => {}
        }
    }
    panic!("waited {WITHIN:?} for pid {pid} to be reaped after the drop");
}

#[test]
fn shared_slots_deliver_remove_and_mark_gone() {
    use super::{Outcome, Shared};
    let shared = Shared::default();
    assert_eq!(shared.next_id(), 1);
    assert_eq!(shared.next_id(), 2);
    shared.insert(7);
    shared.deliver(7, Outcome::Result(json!({})));
    let cancel = fakes::CancelToken::new();
    let seen = shared.view(7, &cancel);
    assert_eq!(seen.response, Some(Outcome::Result(json!({}))));
    assert!(!seen.gone);
    // A late response to a removed id is discarded, never misrouted.
    shared.remove(7);
    shared.deliver(7, Outcome::Result(json!({"late": true})));
    let missing = shared.view(7, &cancel);
    assert_eq!(missing.response, None);
    shared.gone();
    assert!(shared.view(9, &cancel).gone);
}

#[test]
fn server_requests_are_answered_ping_ok_and_unknown_32601() {
    let setup = Setup::tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    write(&setup.dir, "ping-on-start", "");
    let opened = setup.start(Duration::from_secs(5));
    // The fixture's ping answers land in its own log as received lines:
    // `ping` gets `result {}`, the unknown method gets `-32601`.
    let (_held, tick) = mpsc::channel::<()>();
    for _ in 0..POLLS {
        let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
        if log.contains("\"id\":\"probe\"")
            && log.contains("\"result\":{}")
            && log.contains("\"id\":\"bogus\"")
            && log.contains("-32601")
        {
            opened.server.stop();
            return;
        }
        match tick.recv_timeout(POLL) {
            Ok(()) | Err(_) => {}
        }
    }
    panic!("waited {WITHIN:?} for ping answers in requests.log");
}

#[test]
fn stop_leaves_no_running_child() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let pid = setup.pid();
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server runs before the stop",
    );
    opened.server.stop();
    let (_held, probe) = mpsc::channel::<()>();
    for _ in 0..POLLS {
        if !fakes::kill_pid(pid, "0").expect("probe") {
            return;
        }
        match probe.recv_timeout(POLL) {
            Ok(()) | Err(_) => {}
        }
    }
    panic!("waited {WITHIN:?} for pid {pid} to exit after the stop");
}

#[test]
fn a_line_ending_in_a_newline_strips_it() {
    // Without the `ends_with` guard the trailing newline would stay in
    // the line: the `false` mutant returns `"hi\n"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi\n".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a newline-terminated line is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a newline-terminated line is a line, got too long")
        }
    }
}

#[test]
fn a_final_line_without_a_newline_is_a_line() {
    // Without the `ends_with` guard the last byte would be popped as if
    // it were a newline: the `true` mutant returns `"h"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a final line without a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a final line without a newline is a line, got too long")
        }
    }
}

#[test]
fn an_empty_read_is_the_end() {
    use std::io::Cursor;
    let mut reader = Cursor::new(Vec::new());
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("an empty read is the end, got a line"),
        super::ReadLine::TooLong => panic!("an empty read is the end, got too long"),
    }
}

#[test]
fn exactly_max_line_bytes_is_a_line() {
    // `>=` would end the reader here; only `>` lets exactly `MAX_LINE`
    // bytes through as a line.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("exactly MAX_LINE bytes is a line, got the end"),
        super::ReadLine::TooLong => panic!("exactly MAX_LINE bytes is a line, got too long"),
    }
}

#[test]
fn max_line_content_plus_a_newline_is_a_line() {
    // The `take(MAX_LINE + 1)` lets a full line plus its newline through:
    // `-` or `*` in place of `+` truncates it and the assertion on the
    // exact length fails.
    use std::io::Cursor;
    let mut content = vec![b'x'; super::MAX_LINE];
    content.push(b'\n');
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("MAX_LINE bytes plus a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("MAX_LINE bytes plus a newline is a line, got too long")
        }
    }
}

#[test]
fn max_line_plus_one_bytes_is_too_long() {
    // `==` misses this length and `<` ends short lines instead: only `>`
    // ends exactly the lines past the cap.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE + 1];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::TooLong => {}
        super::ReadLine::Line(_) => panic!("MAX_LINE + 1 bytes is too long, got a line"),
        super::ReadLine::Eof => panic!("MAX_LINE + 1 bytes is too long, got the end"),
    }
}

#[test]
fn a_read_error_without_bytes_is_the_end() {
    // The `true` mutant would also end a read that did carry bytes, and
    // the `false` mutant would return an empty line here.
    let mut reader = AlwaysErr;
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("a failed read without bytes is the end, got a line"),
        super::ReadLine::TooLong => {
            panic!("a failed read without bytes is the end, got too long")
        }
    }
}

#[test]
fn a_read_error_after_bytes_keeps_the_line() {
    // The `true` mutant would discard these bytes and report the end.
    let mut reader = DataThenErr {
        data: b"hi".to_vec(),
        done: false,
    };
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a failed read after bytes keeps them, got the end"),
        super::ReadLine::TooLong => {
            panic!("a failed read after bytes keeps them, got too long")
        }
    }
}

#[test]
fn the_wait_ends_on_a_new_response_or_cancel() {
    // All four combinations: `||` into `&&` misses the two mixed rows,
    // and `!=` into `==` flips the two uncancelled rows.
    assert!(!super::should_stop(7, 7, false));
    assert!(super::should_stop(8, 7, false));
    assert!(super::should_stop(7, 7, true));
    assert!(super::should_stop(8, 7, true));
}

/// A reader whose every read fails, carrying no bytes.
struct AlwaysErr;

impl std::io::Read for AlwaysErr {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

impl std::io::BufRead for AlwaysErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Err(std::io::Error::other("boom"))
    }

    fn consume(&mut self, _amount: usize) {}
}

/// A reader that hands over `data` once, then fails: the line reader sees
/// a partial line followed by an error.
struct DataThenErr {
    data: Vec<u8>,
    done: bool,
}

impl std::io::Read for DataThenErr {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::BufRead;
        let chunk = self.fill_buf()?;
        let len = chunk.len().min(buf.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        self.consume(len);
        Ok(len)
    }
}

impl std::io::BufRead for DataThenErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.done {
            Err(std::io::Error::other("boom"))
        } else {
            Ok(&self.data)
        }
    }

    fn consume(&mut self, _amount: usize) {
        self.data.clear();
        self.done = true;
    }
}
