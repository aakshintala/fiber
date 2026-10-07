//! `doors::attach` through its public API (`docs/testing.md`, "Levels"): a
//! client on a running session's socket that prints only its own turn.
//!
//! Every wait in this file has a named deadline: `attach` runs on a helper
//! thread and its result is awaited with `recv_timeout(DEADLINE)`, so a hung
//! attach fails the test instead of hanging it, and the session's inbox
//! thread and close are bounded the same way.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::events::{
    Clients, Empty, Event, FiberExited, InputItem, TurnCompleted, TurnOutcome, TurnStarted,
};
use contract::inbox::{Ack, Delivery, Rejection};
use contract::shapes::{ContentPart, Failure, Origin, Sender, Tokens, Usage};
use contract::{CommandId, ErrorCode, SCHEMA_VERSION, SessionId, TurnId};
use doors::{Session, attach, failure, mint};
use log::Log;
use serde_json::Value;

/// How long one wait may take. Every wait in this file names it.
const DEADLINE: Duration = Duration::from_secs(10);

/// A temporary directory, removed on drop, with a short name: a session's
/// socket path must fit in 103 bytes on macOS.
struct Temp(
    PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
);

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("fa");
        let dir = held.path().to_path_buf();
        Self(dir, held)
    }
}

struct Opened {
    _temp: Temp,
    home: PathBuf,
    id: SessionId,
    socket: PathBuf,
    log: Arc<Log>,
    session: Option<Session>,
}

impl Opened {
    fn open() -> Self {
        let temp = Temp::new();
        let home = temp.0.join("h");
        let sessions = home.join("projects/p/sessions");
        let id = SessionId(mint("s_"));
        let dir = sessions.join(&id.0);
        let clock = fakes::clock::FakeClock::new();
        let log = Arc::new(Log::create(&sessions, id.clone(), clock).unwrap());
        let session = Session::open(
            &home,
            &dir,
            &log,
            fakes::clock::FakeClock::new(),
            Vec::new(),
            Box::new(std::io::sink()),
        )
        .unwrap();
        let socket = temp.0.join("h/run").join(&id.0);
        Self {
            _temp: temp,
            home,
            id: id.clone(),
            socket,
            log,
            session: Some(session),
        }
    }

    /// Runs the session with `behave` on its inbox, on a thread, while the
    /// test attaches. Returns what the inbox thread sends back, bounded by
    /// [`DEADLINE`]: a hung loop fails the test instead of hanging it.
    fn run(
        &mut self,
        behave: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure> + Send + 'static,
    ) -> Receiver<(Result<(), Failure>, Session)> {
        let session = self.session.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let ran = session.run(Vec::new(), Arc::new(|| false), behave);
            match done.send((ran, session)) {
                Ok(()) | Err(_) => {}
            }
        });
        finished
    }

    /// Takes the inbox thread's result within [`DEADLINE`], then closes the
    /// session on a thread, bounded the same way. Consumes the fixture so
    /// the log is dropped: [`Session::close`] joins the printer, which wakes
    /// only once the log is gone.
    fn close(self, inbox: Receiver<(Result<(), Failure>, Session)>) -> Result<(), Failure> {
        let (ran, session) = inbox
            .recv_timeout(DEADLINE)
            .expect("waited for the inbox thread to finish");
        let log = self.log;
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            session.close(log);
            match done.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        finished
            .recv_timeout(DEADLINE)
            .expect("waited for the session to close");
        ran
    }
}

/// Runs `attach` on a helper thread: a hung attach fails the test at the
/// `recv_timeout` below instead of hanging it.
fn attach_on_thread<W: Write + Send + 'static>(
    home: PathBuf,
    id: SessionId,
    prompt: String,
    out: W,
    refused: Failure,
) -> Receiver<(Result<i32, Failure>, W)> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut out = out;
        let code = attach(&home, &id, prompt, &mut out, refused);
        match done.send((code, out)) {
            Ok(()) | Err(_) => {}
        }
    });
    finished
}

/// The helper thread's result, waited for within [`DEADLINE`].
fn attached<T>(
    finished: Receiver<(Result<i32, Failure>, T)>,
    operation: &str,
) -> (Result<i32, Failure>, T) {
    finished.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!("waited {DEADLINE:?} for {operation}");
    })
}

/// The refusal `attach` returns when nothing accepts: the holder names it,
/// which attach never reads.
fn refused(id: &SessionId) -> Failure {
    failure(
        ErrorCode::SessionHeld,
        format!(
            "session {} is held by process 42; only one Fiber process may write a session",
            id.0
        ),
    )
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn turn_input(text: &str, command: &CommandId) -> Vec<InputItem> {
    vec![InputItem::Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(command.clone()),
        },
        changed_by: None,
    }]
}

/// The prompt's delivery, with its acknowledgement for the test to answer
/// once the turn it starts is written, as the loop does: `turn_started`
/// first, then the accept (`loop`, `turn`).
fn prompt_of(inbox: &Receiver<Delivery>) -> (Vec<ContentPart>, CommandId, Ack) {
    match inbox.recv_timeout(DEADLINE).expect("the prompt arrives") {
        Delivery::Prompt(message, ack) => (
            message.content,
            message
                .sender
                .command_id
                .expect("a driver prompt has a command_id"),
            ack,
        ),
        Delivery::Close(_)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Reply(..)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::ExtensionExec(_)
        | Delivery::Cancelled => panic!("the prompt arrives as a prompt"),
    }
}

fn append(log: &Log, event: &Event, turn: &str) {
    log.append(event, Some(TurnId(turn.into())), None).unwrap();
}

fn completed() -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome: TurnOutcome::Completed,
        error: None,
        questions: None,
    })
}

fn kinds(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Parses every line of `text`, failing on a line that is not JSON.
fn parse(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn attach_prints_only_its_turn_and_leaves_the_session_up() {
    let mut opened = Opened::open();
    // An earlier turn, and the `fiber_exited` of the process before the
    // holder: both are in the fold, and neither is printed.
    let old = CommandId("c_old".into());
    append(
        &opened.log,
        &Event::TurnStarted(TurnStarted {
            input: turn_input("before", &old),
        }),
        "t_old",
    );
    append(&opened.log, &Event::StepStarted(Empty {}), "t_old");
    append(&opened.log, &completed(), "t_old");
    opened
        .log
        .append(
            &Event::FiberExited(FiberExited {
                exit_code: 0,
                usage: usage(),
                final_message: None,
                error: None,
                suspended_on: None,
                questions: None,
            }),
            None,
            None,
        )
        .unwrap();
    let log = Arc::clone(&opened.log);
    let inbox = opened.run(move |inbox| {
        let (_, command, ack) = prompt_of(&inbox);
        append(
            &log,
            &Event::TurnStarted(TurnStarted {
                input: turn_input("during", &command),
            }),
            "t_new",
        );
        // The loop accepts the prompt once its `turn_started` is written:
        // the acknowledgement arrives mid-turn and is never printed.
        ack.0(Ok(None));
        // A `clients` line inside the turn's range is part of what `ask`
        // prints, so attach prints it too.
        append(&log, &Event::Clients(Clients { count: 2 }), "t_new");
        append(&log, &Event::StepStarted(Empty {}), "t_new");
        append(&log, &completed(), "t_new");
        assert!(
            inbox.try_recv().is_err(),
            "attach sent no `close`: nothing follows the prompt"
        );
        Ok(())
    });

    let finished = attach_on_thread(
        opened.home.clone(),
        opened.id.clone(),
        "during".into(),
        Vec::new(),
        refused(&opened.id),
    );
    let (code, out) = attached(finished, "attach to return");
    let code = code.unwrap();

    assert_eq!(code, 0);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        kinds(&text),
        ["turn_started", "clients", "step_started", "turn_completed"].map(String::from)
    );
    let lines = parse(&text);
    assert_eq!(
        lines[0]["payload"]["input"][0]["content"][0]["text"],
        "during"
    );
    let command = lines[0]["payload"]["input"][0]["command_id"]
        .as_str()
        .unwrap();
    assert!(command.starts_with("c_"), "attach minted the command id");
    assert_ne!(command, "c_old");
    for line in &lines {
        assert_eq!(line["session_id"], opened.id.0);
    }
    // The turn's own lines are durable; the `clients` line in their midst
    // is ephemeral, as `ask` prints it.
    for line in &lines {
        if line["kind"] == "clients" {
            assert!(line.get("seq").is_none(), "{line}");
        } else {
            assert!(line.get("seq").is_some(), "{line}");
        }
    }
    // Attach sent no `close`, and the session is still up: its socket
    // accepts a connection while the inbox thread is still serving.
    UnixStream::connect(&opened.socket).expect("the session is still up");
    assert_eq!(opened.close(inbox), Ok(()));
}

#[test]
fn a_failed_turn_prints_its_lines_and_returns_1() {
    let mut opened = Opened::open();
    let log = Arc::clone(&opened.log);
    let inbox = opened.run(move |inbox| {
        let (_, command, ack) = prompt_of(&inbox);
        append(
            &log,
            &Event::TurnStarted(TurnStarted {
                input: turn_input("doomed", &command),
            }),
            "t_1",
        );
        ack.0(Ok(None));
        append(
            &log,
            &Event::TurnCompleted(TurnCompleted {
                outcome: TurnOutcome::Failed,
                error: Some(Failure {
                    code: ErrorCode::ToolError,
                    message: "the tool failed".into(),
                    retry_after: None,
                    provider: None,
                }),
                questions: None,
            }),
            "t_1",
        );
        Ok(())
    });

    let finished = attach_on_thread(
        opened.home.clone(),
        opened.id.clone(),
        "doomed".into(),
        Vec::new(),
        refused(&opened.id),
    );
    let (code, out) = attached(finished, "attach to return");
    let code = code.unwrap();

    assert_eq!(code, 1);
    assert_eq!(
        kinds(&String::from_utf8(out).unwrap()),
        ["turn_started", "turn_completed"].map(String::from)
    );
    assert_eq!(opened.close(inbox), Ok(()));
}

#[test]
fn a_prompt_rejected_busy_is_a_failure_printing_nothing() {
    let mut opened = Opened::open();
    let inbox = opened.run(move |inbox| {
        match inbox.recv_timeout(DEADLINE).expect("the prompt arrives") {
            Delivery::Prompt(_, ack) => ack.0(Err(Rejection {
                code: ErrorCode::Busy,
                message: "A turn is running; send `steer` to add to it.".into(),
            })),
            Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Handoff(..)
            | Delivery::Reply(..)
            | Delivery::Cancelled
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::Close(_) => panic!("the prompt arrives as a prompt"),
        }
        Ok(())
    });

    let finished = attach_on_thread(
        opened.home.clone(),
        opened.id.clone(),
        "late".into(),
        Vec::new(),
        refused(&opened.id),
    );
    let (failed, out) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed.code, ErrorCode::Busy);
    assert_eq!(
        failed.message,
        "A turn is running; send `steer` to add to it."
    );
    assert!(out.is_empty(), "a rejection prints nothing");
    assert_eq!(opened.close(inbox), Ok(()));
}

#[test]
fn the_session_ending_before_the_turn_completes_is_a_failure_not_a_hang() {
    let mut opened = Opened::open();
    let log = Arc::clone(&opened.log);
    let inbox = opened.run(move |inbox| {
        let (_, command, ack) = prompt_of(&inbox);
        append(
            &log,
            &Event::TurnStarted(TurnStarted {
                input: turn_input("cut", &command),
            }),
            "t_1",
        );
        ack.0(Ok(None));
        log.append(
            &Event::FiberExited(FiberExited {
                exit_code: 0,
                usage: usage(),
                final_message: None,
                error: None,
                suspended_on: None,
                questions: None,
            }),
            None,
            None,
        )
        .unwrap();
        Ok(())
    });

    let finished = attach_on_thread(
        opened.home.clone(),
        opened.id.clone(),
        "cut".into(),
        Vec::new(),
        refused(&opened.id),
    );
    let (failed, out) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed.code, ErrorCode::Closing);
    let text = String::from_utf8(out).unwrap();
    assert!(text.is_empty() || kinds(&text) == ["turn_started"].map(String::from));
    assert_eq!(opened.close(inbox), Ok(()));
}

/// A stand-in session socket: answers `subscribe`, then `prompt`, then
/// `behave` writes the rest before `drop` closes the connection. Returns
/// the thread with the lines it received, through a channel bounded by
/// [`DEADLINE`] at the join below.
fn stand_in(
    socket: &Path,
    schema_version: u32,
    behave: impl FnOnce(BufReader<UnixStream>, Value, Value) + Send + 'static,
) -> (JoinHandle<()>, Receiver<Vec<String>>) {
    let listener = UnixListener::bind(socket).unwrap();
    let (done, finished) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("attach connected");
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("a read timeout is set");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let mut writer = stream;
        let mut received = Vec::new();
        let mut line = String::new();
        reader.read_line(&mut line).expect("the subscribe arrives");
        received.push(line.clone());
        let sub: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(sub["command"], "subscribe");
        send(
            &mut writer,
            &accepted(&sub, schema_version, &SessionId("s_stand".into())),
        );
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                match done.send(received) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            Ok(_) => received.push(line.clone()),
            Err(_) => {
                match done.send(received) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
        }
        let prompted: Value = serde_json::from_str(line.trim_end()).unwrap();
        if prompted["command"] == "prompt" {
            send(
                &mut writer,
                &accepted(&prompted, schema_version, &SessionId("s_stand".into())),
            );
        }
        behave(reader, sub, prompted);
        match done.send(received) {
            Ok(()) | Err(_) => {}
        }
    });
    (handle, finished)
}

/// The stand-in thread's received lines, waited for within [`DEADLINE`].
/// The thread sends its lines as its last act; the handle is dropped,
/// never joined, so only the deadline bounds its end.
fn stood_in(server: (JoinHandle<()>, Receiver<Vec<String>>), operation: &str) -> Vec<String> {
    let (_handle, finished) = server;
    finished.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        panic!("waited {DEADLINE:?} for {operation}");
    })
}

fn accepted(command: &Value, schema_version: u32, session: &SessionId) -> String {
    serde_json::to_string(&serde_json::json!({
        "kind": "command_accepted",
        "session_id": session.0,
        "ts": 0,
        "schema_version": schema_version,
        "payload": {"command_id": command["id"]},
    }))
    .unwrap()
}

fn send(writer: &mut UnixStream, line: &str) {
    writer.write_all(line.as_bytes()).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

#[test]
fn a_different_schema_version_is_session_held_naming_both_versions() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let id = SessionId(mint("s_"));
    let socket = run.join(&id.0);
    let server = stand_in(&socket, SCHEMA_VERSION + 1, |_, _, _| {});

    let finished = attach_on_thread(
        home.clone(),
        id.clone(),
        "hi".into(),
        Vec::new(),
        refused(&id),
    );
    let (failed, out) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed.code, ErrorCode::SessionHeld);
    assert!(
        failed.message.contains(&id.0),
        "the message names the session: {}",
        failed.message
    );
    assert!(
        failed.message.contains(&(SCHEMA_VERSION + 1).to_string()),
        "the message names the session's version: {}",
        failed.message
    );
    assert!(
        failed.message.contains(&SCHEMA_VERSION.to_string()),
        "the message names this build's version: {}",
        failed.message
    );
    assert!(out.is_empty());
    // Attach sent no prompt after the version check: the stand-in saw the
    // subscribe alone, then the connection's end.
    let received = stood_in(server, "the stand-in to see the connection end");
    assert_eq!(received.len(), 1);
    assert!(received[0].contains("subscribe"));
}

#[test]
fn a_connection_closed_before_the_turn_completes_is_a_failure() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let id = SessionId(mint("s_"));
    let socket = run.join(&id.0);
    // The stand-in answers both commands, then drops the connection with no
    // turn and no `fiber_exited`.
    let server = stand_in(&socket, SCHEMA_VERSION, |_, _, _| {});

    let finished = attach_on_thread(
        home.clone(),
        id.clone(),
        "hi".into(),
        Vec::new(),
        refused(&id),
    );
    let (failed, out) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed.code, ErrorCode::Closing);
    assert!(out.is_empty());
    stood_in(server, "the stand-in to see the connection end");
}

#[test]
fn a_socket_that_refuses_returns_the_refusal() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let sessions = home.join("projects/p/sessions");
    let id = SessionId(mint("s_"));
    // The lock is held, as a live session would hold it, but nothing
    // accepts on the socket.
    let _log = Log::create(&sessions, id.clone(), fakes::clock::FakeClock::new()).unwrap();

    let expected = refused(&id);
    let finished = attach_on_thread(
        home.clone(),
        id.clone(),
        "hi".into(),
        Vec::new(),
        expected.clone(),
    );
    let (failed, out) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed, expected);
    assert!(out.is_empty());
}

/// A writer that fails every write.
struct Broken;

impl Write for Broken {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("closed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("closed"))
    }
}

#[test]
fn an_unwritable_stdout_is_an_io_failure() {
    let mut opened = Opened::open();
    let log = Arc::clone(&opened.log);
    let inbox = opened.run(move |inbox| {
        let (_, command, ack) = prompt_of(&inbox);
        append(
            &log,
            &Event::TurnStarted(TurnStarted {
                input: turn_input("lost", &command),
            }),
            "t_1",
        );
        ack.0(Ok(None));
        append(&log, &completed(), "t_1");
        Ok(())
    });

    let finished = attach_on_thread(
        opened.home.clone(),
        opened.id.clone(),
        "lost".into(),
        Broken,
        refused(&opened.id),
    );
    let (failed, _) = attached(finished, "attach to return");
    let failed = failed.unwrap_err();

    assert_eq!(failed.code, ErrorCode::IoFailed);
    assert_eq!(opened.close(inbox), Ok(()));
}
