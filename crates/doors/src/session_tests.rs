//! Connections leaving, a lagging connection, and close joining a reader.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{
    CommandAccepted, Empty, Event, ExtensionsLoaded, FiberExited, LoadedExtension, Notice,
    SessionState, SessionStatus, ToolState,
};
use contract::inbox::Delivery;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Process, Tokens, Usage};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, ErrorCode};
use fakes::Client;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value};

use super::{Gate, Session};

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::{DEADLINE, Opened, close_within, subscribe_line};

/// How long a held writer stays blocked in `write`: far past the test's own
/// waits, so when close never releases it the test's deadline names the hang
/// instead of the writer giving up and letting close return early.
const WRITE_HOLD: Duration = Duration::from_secs(30);

struct Control {
    hold_reader: AtomicBool,
    hold_mu: Mutex<()>,
    hold_cv: Condvar,
    parked: AtomicUsize,
    parked_mu: Mutex<()>,
    parked_cv: Condvar,
    accept_waiting: AtomicBool,
    accept_mu: Mutex<()>,
    accept_cv: Condvar,
    switch_waiting: AtomicBool,
    switch_mu: Mutex<()>,
    switch_cv: Condvar,
    skip_shutdown: AtomicBool,
}

static CONTROL: Control = Control {
    hold_reader: AtomicBool::new(false),
    hold_mu: Mutex::new(()),
    hold_cv: Condvar::new(),
    parked: AtomicUsize::new(0),
    parked_mu: Mutex::new(()),
    parked_cv: Condvar::new(),
    accept_waiting: AtomicBool::new(false),
    accept_mu: Mutex::new(()),
    accept_cv: Condvar::new(),
    switch_waiting: AtomicBool::new(false),
    switch_mu: Mutex::new(()),
    switch_cv: Condvar::new(),
    skip_shutdown: AtomicBool::new(false),
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits for the test to release the admission lock (`what` names which
/// wait), failing at [`DEADLINE`]. The probe arms below run on the loop's
/// thread, so they call this instead of panicking in the closure.
#[track_caller]
fn released(release: &Mutex<mpsc::Receiver<()>>, what: &str) {
    Deadline::after(DEADLINE)
        .recv(&lock(release))
        .unwrap_or_else(|_| panic!("the test releases {what}"));
}

fn wait_until_accept_waits() {
    let guard = lock(&CONTROL.accept_mu);
    let (_guard, _) = CONTROL
        .accept_cv
        .wait_timeout_while(guard, DEADLINE, |_| {
            !CONTROL.accept_waiting.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.accept_waiting.load(Ordering::Relaxed),
        "accept waits after an error"
    );
}

pub(super) fn park_reader() {
    if !CONTROL.hold_reader.load(Ordering::Relaxed) {
        return;
    }
    {
        let _held = lock(&CONTROL.parked_mu);
        CONTROL.parked.fetch_add(1, Ordering::Relaxed);
        CONTROL.parked_cv.notify_all();
    }
    let hold = lock(&CONTROL.hold_mu);
    let (_hold, _) = CONTROL
        .hold_cv
        .wait_timeout_while(hold, DEADLINE, |_| {
            CONTROL.hold_reader.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !CONTROL.hold_reader.load(Ordering::Relaxed),
        "the reader is still held at exit"
    );
}

pub(super) fn shutdown_skipped() -> bool {
    CONTROL.skip_shutdown.load(Ordering::Relaxed)
}

/// A point a test observes through [`Gate::probe`]..
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Probe {
    /// The first wait for the driver shells has returned: the session
    /// enters its answer phase next.
    FirstShellWaitDone,
    /// A writer saw STOP while the session is stopping and waits instead
    /// of ending.
    WriterParkedAtStop,
    /// The writer reached past the log's end and reads its next line
    /// there: after this point a pushed line arrives past the end.
    WriterPastEnd,
    /// `close` has queued its `STOP` for the printer.
    PrinterStopQueued,
    /// A wait for the driver shells is about to block on one still running.
    ShellsWaiting,
    /// A `full` subscribe has its watcher and seed queued, before the writer
    /// starts.
    SubscribeSeeded,
    /// A `full` subscribe's acknowledgement is written.
    SubscribeAcknowledged,
    /// `deliver_message` holds the admission lock while sending.
    MessageLocked,
    /// `mark_closed` holds the admission lock while closing.
    CloseLocked,
}

/// What [`Gate::probe`] calls at each [`Probe`] point.
pub(crate) type Prober = Arc<dyn Fn(Probe) + Send + Sync>;

pub(super) fn note_accept_wait() {
    let _held = lock(&CONTROL.accept_mu);
    CONTROL.accept_waiting.store(true, Ordering::Relaxed);
    CONTROL.accept_cv.notify_all();
}

/// Notes that a connection's writer parked at a label-bearing switch's
/// deferred response wait: the writer thread calls this immediately
/// before blocking on the loop's answer, so a test that receives it
/// knows no later line can reach the client until the test answers the
/// delivered acknowledgement (`docs/testing.md`, "Waits and timeouts").
pub(crate) fn note_switch_wait() {
    let _held = lock(&CONTROL.switch_mu);
    CONTROL.switch_waiting.store(true, Ordering::Relaxed);
    CONTROL.switch_cv.notify_all();
}

/// Waits until the connection's writer parked at the switch's response
/// wait, failing at [`DEADLINE`]: after this returns, silence on the
/// socket is the writer waiting, not a thread that never ran.
#[track_caller]
pub(crate) fn wait_switch_waiting() {
    let guard = lock(&CONTROL.switch_mu);
    let (_guard, _) = CONTROL
        .switch_cv
        .wait_timeout_while(guard, DEADLINE, |_| {
            !CONTROL.switch_waiting.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.switch_waiting.load(Ordering::Relaxed),
        "the writer parked at the switch's response wait"
    );
}

/// Socket-command refusal tests at the dispatch seam: a driven connection
/// carrying fabricated caller attribution, with the target reads scripted
/// through the gate seam. Refusals answer synchronously; the allowed cases
/// assert that nothing is rejected.
mod refusal_tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::{Gate, reset};
    use contract::commands::{
        Command, CommandLine, ContentArgs, Message as MessageArgs, Reply, ReplyAnswer, SentPart,
    };
    use contract::events::Decision;
    use contract::{CommandId, ErrorCode, RequestId, SessionId};
    use fakes::Deadline;

    const PEER: u32 = 555;
    const HOST: u32 = 440;
    const UNREADABLE: u32 = 999;

    /// Scripted kernel reads: the refusal paths answer from these tables,
    /// and a missing entry reads as unreadable.
    #[derive(Default)]
    struct Script {
        current: u32,
        parents: HashMap<u32, u32>,
        euids: HashMap<u32, u32>,
        argvs: HashMap<u32, Vec<Vec<u8>>>,
    }

    impl ::support::peer::ProcessRead for Script {
        fn current_euid(&self) -> Result<u32, ::support::peer::ReadError> {
            Ok(self.current)
        }

        fn euid(&self, pid: u32) -> Result<u32, ::support::peer::ReadError> {
            self.euids
                .get(&pid)
                .copied()
                .ok_or(::support::peer::ReadError::Unreadable { pid, what: "euid" })
        }

        fn parent(&self, pid: u32) -> Result<u32, ::support::peer::ReadError> {
            self.parents
                .get(&pid)
                .copied()
                .ok_or(::support::peer::ReadError::Unreadable {
                    pid,
                    what: "parent",
                })
        }

        fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, ::support::peer::ReadError> {
            self.argvs
                .get(&pid)
                .cloned()
                .ok_or(::support::peer::ReadError::Unreadable { pid, what: "argv" })
        }
    }

    /// The target's walk with no session above it: the target session
    /// itself decides. The target is always this test process.
    fn plain_target() -> Script {
        let own = std::process::id();
        Script {
            current: 1000,
            parents: [(own, 1), (1, 0)].into_iter().collect(),
            euids: [(own, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(1, vec![b"/sbin/init".to_vec()])].into_iter().collect(),
        }
    }

    /// The target's walk with a session host above this test process: the
    /// target shares that session process with a caller below it.
    fn shared_target() -> Script {
        let own = std::process::id();
        Script {
            current: 1000,
            parents: [(own, HOST), (HOST, 1), (1, 0)].into_iter().collect(),
            euids: [(HOST, 1000), (1, 1000)].into_iter().collect(),
            argvs: [
                (
                    HOST,
                    [
                        "/path/to/fiber",
                        "session",
                        "--id=s_0123456789abcdef",
                        "--workspace=/w",
                    ]
                    .iter()
                    .map(|word| word.as_bytes().to_vec())
                    .collect(),
                ),
                (1, vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
        }
    }

    /// The target's readable walk with the peer as its kernel parent: the
    /// direct-parent exception admits every command but an approval answer.
    fn parent_target() -> Script {
        let own = std::process::id();
        Script {
            current: 1000,
            parents: [(own, PEER), (PEER, 1), (1, 0)].into_iter().collect(),
            euids: [(PEER, 1000), (1, 1000)].into_iter().collect(),
            argvs: [
                (PEER, vec![b"/bin/sh".to_vec()]),
                (1, vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn local(peer: u32, matches: &[u32]) -> crate::client::Attribution {
        crate::client::Attribution::Local {
            peer_pid: peer,
            session_process_pids: matches.to_vec(),
        }
    }

    fn steered(id: &str) -> CommandLine {
        CommandLine {
            id: CommandId(id.into()),
            command: Command::Steer(ContentArgs {
                content: vec![SentPart::Text { text: "hi".into() }],
            }),
        }
    }

    fn approval_answered(id: &str) -> CommandLine {
        CommandLine {
            id: CommandId(id.into()),
            command: Command::Reply(Reply {
                request_id: RequestId("r_1".into()),
                answer: ReplyAnswer::Approval {
                    decision: Decision::Allow,
                    feedback: None,
                    remember: None,
                },
            }),
        }
    }

    fn question_answered(id: &str) -> CommandLine {
        CommandLine {
            id: CommandId(id.into()),
            command: Command::Reply(Reply {
                request_id: RequestId("r_1".into()),
                answer: ReplyAnswer::Confirmed { confirmed: true },
            }),
        }
    }

    fn messaged(id: &str, target: &SessionId) -> CommandLine {
        CommandLine {
            id: CommandId(id.into()),
            command: Command::Message(MessageArgs {
                from_session_id: target.clone(),
                parent_session_id: None,
                text: "hi".into(),
            }),
        }
    }

    fn attributed(
        gate: &Arc<Gate>,
        attribution: crate::client::Attribution,
    ) -> (
        crate::client::Conn,
        std::sync::mpsc::Receiver<contract::inbox::Answer>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let conn = crate::client::Conn {
            id: 0,
            gate: Arc::clone(gate),
            sink: crate::client::Sink::Driven {
                answer: contract::inbox::Ack(Box::new(move |answer| {
                    tx.send(answer).unwrap_or(());
                })),
                origin: contract::shapes::Origin::Driver,
            },
            full: true,
            gone: false,
            attribution: Some(attribution),
        };
        (conn, rx)
    }

    fn scripted(gate: &Arc<Gate>, script: Script) {
        *::support::lock(&gate.test_read) = Some(Arc::new(script));
    }

    fn rejected(answer: contract::inbox::Answer) -> contract::inbox::Rejection {
        match answer {
            Err(rejection) => rejection,
            Ok(_) => panic!("a refusal was expected: {answer:?}"),
        }
    }

    #[test]
    fn an_approval_answer_from_a_session_caller_is_refused_on_an_unrelated_session() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, plain_target());
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, approval_answered("c_1"), "reply");
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("the refusal is answered");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::FromSession);
        assert!(
            rejection.message.contains("inside a session"),
            "{}",
            rejection.message
        );
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn a_question_answer_on_an_unrelated_session_stays_allowed() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, plain_target());
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, question_answered("c_1"), "reply");
        // The refusal check passed: without a running loop the idle inbox
        // ends delivery `Closing`, never `from_session`.
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("delivery ends the command");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::Closing);
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn a_question_answer_sharing_a_session_process_is_refused() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, shared_target());
        let target = gate.session_id.clone();
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, question_answered("c_1"), "reply");
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("the refusal is answered");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::FromSession);
        assert!(
            rejection.message.contains(&target.0),
            "{}",
            rejection.message
        );
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn a_message_to_self_is_not_refused() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, shared_target());
        let target = gate.session_id.clone();
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, messaged("c_1", &target), "message");
        // A message is never refused: without a running loop the idle
        // inbox ends delivery `Closing`, never `from_session`.
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("delivery ends the command");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::Closing);
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn a_failed_credential_read_renders_its_error() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, plain_target());
        let attribution = crate::client::Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some("injected credential failure".to_owned()),
        };
        let (mut conn, rx) = attributed(&gate, attribution);
        crate::client::dispatch(&mut conn, steered("c_1"), "steer");
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("the refusal is answered");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::FromSession);
        assert!(
            rejection.message.contains("injected credential failure"),
            "{}",
            rejection.message
        );
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn an_unreadable_target_names_the_target_session() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, Script::default());
        let target = gate.session_id.clone();
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, steered("c_1"), "steer");
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("the refusal is answered");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::FromSession);
        assert!(
            rejection.message.contains(&target.0),
            "{}",
            rejection.message
        );
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn the_direct_parent_reaches_despite_an_incomplete_walk() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, parent_target());
        let attribution = crate::client::Attribution::Incomplete {
            peer_pid: Some(PEER),
            pid: Some(UNREADABLE),
            peer_error: None,
        };
        let (mut conn, rx) = attributed(&gate, attribution);
        crate::client::dispatch(&mut conn, steered("c_1"), "steer");
        // The exception allowed the steer: without a running loop the idle
        // inbox ends delivery `Closing`, never `from_session`.
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("delivery ends the command");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::Closing);
        super::support::close_within(opened.session, opened.log);
    }

    #[test]
    fn an_approval_answer_has_no_parent_exception() {
        reset();
        let opened = super::support::Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        scripted(&gate, parent_target());
        let (mut conn, rx) = attributed(&gate, local(PEER, &[HOST]));
        crate::client::dispatch(&mut conn, approval_answered("c_1"), "reply");
        let answer = Deadline::after(super::support::DEADLINE)
            .recv(&rx)
            .expect("the refusal is answered");
        let rejection = rejected(answer);
        assert_eq!(rejection.code, ErrorCode::FromSession);
        assert!(
            rejection.message.contains("inside a session"),
            "{}",
            rejection.message
        );
        super::support::close_within(opened.session, opened.log);
    }
}

fn reset() {
    CONTROL.hold_reader.store(false, Ordering::Relaxed);
    CONTROL.hold_cv.notify_all();
    CONTROL.parked.store(0, Ordering::Relaxed);
    CONTROL.accept_waiting.store(false, Ordering::Relaxed);
    CONTROL.switch_waiting.store(false, Ordering::Relaxed);
    CONTROL.skip_shutdown.store(false, Ordering::Relaxed);
}

struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
    }
}

fn notice() -> Event {
    Event::Notice(Notice {
        code: ErrorCode::IoFailed,
        message: "n".into(),
        extension: None,
    })
}

fn extensions() -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: "demo".into(),
            version: "1".into(),
            replaces: Vec::new(),
        }],
    })
}

fn recv(client: &Client) -> serde_json::Value {
    client
        .recv(fakes::Deadline::after(DEADLINE))
        .expect("a line arrived before the deadline")
}

fn descriptors() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

/// Waits until no connection is live, no writer is open and the process
/// holds `before` descriptors. A connection leaves `live` before `reap` drops
/// its socket's last clone; `Gate::finish` notifies under the lock after that,
/// so the count, not an empty `live`, proves the descriptors are released.
fn wait_released(gate: &Gate, before: usize) {
    assert!(
        gate.conns.wait_while(DEADLINE, |state| {
            state.live_len() != 0 || state.writers_open() > 0 || descriptors() != before
        }),
        "descriptors return to the count from before the clients connected"
    );
}

#[test]
fn many_connections_leave_nothing_held() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let before = descriptors();
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_live", "full")).unwrap();
            let _ack = recv(&client);
            {
                assert!(
                    gate.conns
                        .wait_while(DEADLINE, |state| !state.has_full_entry()),
                    "a live connection keeps its reader, its writer and its shutdown"
                );
            }
            drop(client);
            let socket = socket.clone();
            fakes::within("forty connections", Deadline::after(DEADLINE), move || {
                for i in 0..40 {
                    let client = Client::connect(&socket).unwrap();
                    client
                        .send(&subscribe_line(&format!("c_{i}"), "full"))
                        .unwrap();
                    let _ack = recv(&client);
                    drop(client);
                }
            });
            wait_released(&gate, before);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn close_joins_a_reader_that_is_still_connected() {
    reset();
    let _release = Release;
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "full")).unwrap();
            let _ack = recv(&client);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client subscribed");
    CONTROL.hold_reader.store(true, Ordering::Relaxed);
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    {
        let parked = lock(&CONTROL.parked_mu);
        let (_parked, _) = CONTROL
            .parked_cv
            .wait_timeout_while(parked, DEADLINE, |_| {
                CONTROL.parked.load(Ordering::Relaxed) == 0
            })
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            CONTROL.parked.load(Ordering::Relaxed) > 0,
            "the reader reaches its exit while close joins it"
        );
    }
    assert!(
        done_rx.try_recv().is_err(),
        "close returns before the reader has finished"
    );
    {
        let _hold = lock(&CONTROL.hold_mu);
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
    }
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returns after the reader finishes");
    drop(client);
}

/// How long each slice of `wait_reader_published` waits: `push_reader`
/// publishes without a notify, so the wait re-checks in slices under one
/// deadline instead of parking on the missed wake.
const READER_SLICE: Duration = Duration::from_millis(10);

/// Waits for the silent client's reader to be published and blocked: one
/// deadline across short condvar slices, since the publication carries no
/// wake. The failure message is the test's own.
#[track_caller]
fn wait_reader_published(gate: &Gate) {
    let deadline = Deadline::after(DEADLINE);
    loop {
        if gate
            .conns
            .wait_while(READER_SLICE, |state| !state.has_reader_shutdown())
        {
            return;
        }
        assert!(
            !deadline.left().is_zero(),
            "the silent client's reader is published and blocked"
        );
    }
}

#[test]
fn close_returns_while_a_silent_client_stays_open() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client connected");
    // The reader is published and blocked: it holds its reader and its
    // shutdown, and the client sends nothing from here on.
    {
        wait_reader_published(&gate);
    }
    CONTROL.skip_shutdown.store(true, Ordering::Relaxed);
    close_within(opened.session, opened.log);
    drop(client);
}

#[test]
fn a_lagging_connection_does_not_hide_the_latest_from_a_subscriber() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.append(&status("stale"), None, None).unwrap();
            let watcher = log.watch();
            let held = Held::new(Hold::Wait);
            attach_held(&gate, &held, watcher);
            log.append(&notice(), None, None).unwrap();
            held.wait_blocked();
            for _ in 0..1_200 {
                log.append(&notice(), None, None).unwrap();
            }
            log.append(&status("newest"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sum", "summary")).unwrap();
            assert_eq!(recv(&client)["payload"]["command_id"], "c_sum");
            let status_line = recv(&client);
            assert_eq!(status_line["kind"], "session_status");
            assert_eq!(
                status_line["payload"]["name"], "newest",
                "a status emitted after a connection's queue is full still reaches a later subscriber"
            );
            let extensions_line = recv(&client);
            assert_eq!(extensions_line["kind"], "extensions_loaded");
            assert_eq!(
                extensions_line["payload"]["extensions"][0]["name"], "demo",
                "a summary subscriber is sent the latest extensions"
            );
            held.release();
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

fn step() -> Event {
    Event::StepStarted(Empty {})
}

enum Hold {
    Wait,
    Go,
    Fail,
}

struct HeldState {
    entered: bool,
    hold: Hold,
}

struct Held {
    mu: Mutex<HeldState>,
    cv: Condvar,
    buf: Mutex<Vec<u8>>,
    buf_cv: Condvar,
}

struct HeldWrite {
    held: Arc<Held>,
}

impl Held {
    fn new(hold: Hold) -> Arc<Self> {
        Arc::new(Self {
            mu: Mutex::new(HeldState {
                entered: false,
                hold,
            }),
            cv: Condvar::new(),
            buf: Mutex::new(Vec::new()),
            buf_cv: Condvar::new(),
        })
    }

    fn fail(&self) {
        lock(&self.mu).hold = Hold::Fail;
        self.cv.notify_all();
    }

    fn release(&self) {
        lock(&self.mu).hold = Hold::Go;
        self.cv.notify_all();
    }

    fn wait_blocked(&self) {
        let guard = lock(&self.mu);
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, DEADLINE, |state| !state.entered)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(guard.entered, "the writer is blocked in write");
    }

    fn wait_text(&self, ready: impl Fn(&str) -> bool, what: &str) -> String {
        let guard = lock(&self.buf);
        let (guard, _) = self
            .buf_cv
            .wait_timeout_while(guard, DEADLINE, |buf| !ready(&String::from_utf8_lossy(buf)))
            .unwrap_or_else(PoisonError::into_inner);
        let text = String::from_utf8_lossy(&guard).into_owned();
        assert!(ready(&text), "{what}");
        text
    }
}

impl Write for HeldWrite {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut state = lock(&self.held.mu);
        state.entered = true;
        self.held.cv.notify_all();
        let (state, waited) = self
            .held
            .cv
            .wait_timeout_while(state, WRITE_HOLD, |state| matches!(state.hold, Hold::Wait))
            .unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() && matches!(state.hold, Hold::Wait) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the writer was not released",
            ));
        }
        match state.hold {
            Hold::Fail => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the connection shut down",
            )),
            Hold::Wait => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the writer was not released",
            )),
            Hold::Go => {
                drop(state);
                lock(&self.held.buf).extend_from_slice(data);
                self.held.buf_cv.notify_all();
                Ok(data.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn lines_of(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn attach_held(gate: &Arc<super::Gate>, held: &Arc<Held>, watcher: log::Watcher) {
    let (stop_tx, stop_rx) = mpsc::channel();
    let (id_tx, id_rx) = mpsc::channel();
    let injector = watcher.injector();
    let leaving = Arc::clone(gate);
    // The reader holds until the ender sends or is dropped (the readers
    // are quieted), then ends as a real one does: it stops its writer and
    // finishes its connection.
    let reader = thread::spawn(move || {
        match stop_rx.recv() {
            Ok(()) | Err(_) => {}
        }
        injector.push_kept(crate::client::control_line(
            &leaving.session_id,
            crate::client::STOP,
            0,
        ));
        if let Ok(id) = id_rx.recv() {
            leaving.conns.finish(id);
        }
    });
    let fail = Arc::clone(held);
    let shut = Arc::clone(held);
    let id = gate
        .conns
        .push_reader(
            reader,
            Box::new(move || {
                fail.fail();
                if let Ok(()) = stop_tx.send(()) {}
            }),
            Box::new(move || shut.fail()),
        )
        .expect("the gate is running");
    if let Ok(()) = id_tx.send(id) {}
    crate::client::spawn_writer(
        Arc::clone(gate),
        id,
        watcher,
        Box::new(HeldWrite {
            held: Arc::clone(held),
        }),
        false,
        None,
    );
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn exited() -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    })
}

#[test]
fn a_disconnect_stops_a_writer_whose_queue_is_full() {
    reset();
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), timed).unwrap());
    let watcher = log.watch();
    let injector = watcher.injector();
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    injector.push_kept(crate::client::control_line(&id, crate::client::STOP, 0));
    let (mut peer, stream) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match peer.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(stream), false, mpsc::channel().1, None);
        if let Ok(()) = tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the writer stops when its queue was full");
    drop(log);
}

#[test]
fn an_acknowledgement_survives_a_lagged_queue() {
    reset();
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), timed).unwrap());
    let watcher = log.watch();
    let injector = watcher.injector();
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    log.append(&step(), None, None).unwrap();
    let ack = crate::session::envelope(
        &id,
        clock.as_ref(),
        &Event::CommandAccepted(CommandAccepted {
            command_id: CommandId("c_tools".into()),
            result: None,
        }),
    );
    injector.push_kept(ack);
    let held = Held::new(Hold::Go);
    let write = HeldWrite {
        held: Arc::clone(&held),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(write), false, mpsc::channel().1, None);
        if let Ok(()) = tx.send(()) {}
    });
    let text = held.wait_text(
        |text| text.contains("c_tools") && text.contains("step_started"),
        "the acknowledgement and the catch-up line arrive",
    );
    let ack_at = text
        .find("c_tools")
        .expect("the acknowledgement arrives from a lagged queue");
    let step_at = text
        .find("step_started")
        .expect("catch-up returns the durable line the queue dropped");
    assert!(
        ack_at < step_at,
        "a kept acknowledgement is returned before catch-up"
    );
    drop(log);
    injector.push_kept(crate::client::control_line(&id, crate::client::STOP, 0));
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the writer ends once the log is dropped and it is told to stop");
}

#[test]
fn a_blocked_writer_holds_close_until_the_grace_passes() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&notice(), None, None).unwrap();
    let held = Held::new(Hold::Wait);
    attach_held(&gate, &held, watcher);
    held.wait_blocked();
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    assert!(
        clock.await_parked(until, fakes::Deadline::after(DEADLINE)),
        "close waits for a writer that is blocked in write"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned before the grace passes"
    );
    let mark = clock.advance_when_parked(
        Some(until),
        None,
        super::GRACE.saturating_sub(Duration::from_millis(1)),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(until), fakes::Deadline::after(DEADLINE)),
        "close waits again one millisecond short of the grace"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned one millisecond before the grace"
    );
    clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    // The grace cut the writer, but it is blocked in `write` and cannot
    // flush: close waits the flush margin, then shuts its socket.
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned before the flush margin passes"
    );
    clock.advance_when_parked(
        Some(until + super::CUT_FLUSH),
        None,
        super::CUT_FLUSH,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returns once the grace and the flush margin have passed");
}

#[test]
fn should_wait_returns_at_once_with_nothing_owed_and_at_the_grace() {
    let clock = FakeClock::new();
    let now = clock.now();
    let until = now + super::GRACE;
    let before = until
        .checked_sub(Duration::from_millis(1))
        .expect("the grace exceeds a millisecond");
    let after = until + Duration::from_millis(1);
    // Nothing owed: the fast-fail path, on both sides of the deadline.
    assert!(!super::conns::should_wait(0, before, until));
    assert!(!super::conns::should_wait(0, until, until));
    assert!(!super::conns::should_wait(0, after, until));
    // An owed answer waits only before the deadline; the deadline itself
    // is already past the grace.
    assert!(super::conns::should_wait(1, before, until));
    assert!(!super::conns::should_wait(1, until, until));
    assert!(!super::conns::should_wait(1, after, until));
    // The count's width changes nothing: owed is owed.
    assert!(super::conns::should_wait(usize::MAX, before, until));
    assert!(!super::conns::should_wait(usize::MAX, until, until));
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it: taking the cut's ids wakes a wait for owed answers, so the waiter sees the cut instead of the grace.
#[test]
fn cut_wakes_a_waiter_for_owed_answers() {
    reset();
    let accepted = super::Accepted::new();
    let reader = thread::current().id();
    accepted.track(reader);
    accepted.reserve(&CommandId("c_1".into()));
    let clock = FakeClock::new();
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let waiter_clock = Arc::clone(&clock);
    thread::scope(|scope| {
        scope.spawn(|| {
            accepted.wait_owed(waiter_clock.as_ref(), until);
            if let Ok(()) = done_tx.send(()) {}
        });
        assert!(
            clock.await_parked(until, Deadline::after(DEADLINE)),
            "wait_owed parks while an answer is owed"
        );
        accepted.cut(reader);
        let done = Deadline::after(DEADLINE).recv(&done_rx);
        // Release the waiter either way, so a failure here leaves no
        // parked thread for the scope to join forever.
        accepted.untrack(reader);
        assert!(done.is_ok(), "a cut wakes the wait for owed answers");
    });
}

#[test]
fn a_published_reader_is_shut_down_before_it_is_joined() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (peer, mut stream) = UnixStream::pair().unwrap();
    let peer = Mutex::new(Some(peer));
    let shut = Arc::new(AtomicBool::new(false));
    let reader = thread::spawn(move || {
        let mut buf = [0u8; 1];
        let ended = stream.read(&mut buf);
        drop(ended);
    });
    // The shutdown ends the reader by EOF, so it works whether the reader
    // has reached `read` yet or not.
    let flag = Arc::clone(&shut);
    let id = gate
        .conns
        .push_reader(
            reader,
            Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                drop(lock(&peer).take());
            }),
            Box::new(|| {}),
        )
        .expect("the gate is running");
    assert!(
        gate.conns.published(id),
        "the reader is published under its id"
    );
    let (done_tx, done_rx) = mpsc::channel();
    let finishing = Arc::clone(&gate);
    thread::spawn(move || {
        finishing.conns.finish(id);
        if let Ok(()) = done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("reap shuts the reader down before joining it");
    assert!(shut.load(Ordering::SeqCst), "reap ran the shutdown");
    close_within(opened.session, opened.log);
}

#[test]
fn a_reader_that_reaps_itself_finishes() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (peer, stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let shut = stream.try_clone().unwrap();
    let peer_pid = ::support::peer::peer_pid(&stream);
    let (read, stop) = ::support::stoppable::reader(stream).unwrap();
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let child = Arc::clone(&gate);
    let reader = thread::spawn(move || {
        let Ok(id) = Deadline::after(DEADLINE).recv(&id_rx) else {
            return;
        };
        crate::client::serve(read, child, id, peer_pid);
        if let Ok(()) = done_tx.send(()) {}
    });
    let id = gate
        .conns
        .push_reader(
            reader,
            crate::client::ender(shutdown, stop),
            super::conns::shutter(shut),
        )
        .expect("the gate is running");
    if let Ok(()) = id_tx.send(id) {}
    drop(peer);
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("a reader that reaps itself finishes");
    close_within(opened.session, opened.log);
}

#[test]
fn a_reading_writer_records_fiber_exited_before_it_ends() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&exited(), None, None).unwrap();
    let held = Held::new(Hold::Go);
    attach_held(&gate, &held, watcher);
    held.wait_text(
        |text| text.contains("fiber_exited"),
        "a reading writer records fiber_exited",
    );
    {
        assert!(
            gate.conns.writers_open() > 0,
            "fiber_exited arrives before the writer ends"
        );
    }
    close_within(opened.session, opened.log);
}

#[test]
fn a_slow_writer_keeps_durable_lines_in_seq_order() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&notice(), None, None).unwrap();
    let held = Held::new(Hold::Wait);
    attach_held(&gate, &held, watcher);
    held.wait_blocked();
    let log = Arc::clone(&opened.log);
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    for _ in 0..3 {
        log.append(&step(), None, None).unwrap();
    }
    drop(log);
    held.release();
    let text = held.wait_text(
        |text| lines_of(text).iter().any(|line| line["seq"] == 2),
        "the durable lines arrive after the writer is released",
    );
    let lines = lines_of(&text);
    let durable: Vec<&str> = lines
        .iter()
        .filter(|line| line["seq"].is_u64())
        .filter_map(|line| line["kind"].as_str())
        .collect();
    assert_eq!(
        durable,
        ["step_started", "step_started", "step_started"],
        "durable lines arrive in seq order"
    );
    let seqs: Vec<u64> = lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect();
    assert_eq!(seqs, vec![0, 1, 2]);
    let notices = lines.iter().filter(|line| line["kind"] == "notice").count();
    assert!(
        notices < 1_200,
        "a slow writer drops ephemeral lines, got {notices}"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn accept_waits_after_an_error_until_a_connection_ends() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let waiter = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        waiter.conns.wait_for_room();
        if let Ok(()) = tx.send(()) {}
    });
    wait_until_accept_waits();
    assert!(
        rx.try_recv().is_err(),
        "accept waits until a connection ends or the session stops"
    );
    gate.conns.end_writer();
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("a connection ending lets accept try again");
    close_within(opened.session, opened.log);
}

#[test]
fn accept_waits_for_room_on_a_non_interrupted_error() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // A socket that never listened: every accept fails at once with
    // `EINVAL`, a non-`Interrupted` error, on every platform, with no timing
    // involved. (`Interrupted` has no deterministic source here: it needs a
    // signal delivered mid-accept, which the suite never stages.)
    let socket = rustix::net::socket(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        None,
    )
    .unwrap();
    let listener = UnixListener::from(socket);
    let worker = thread::Builder::new()
        .name("accept-test-error".to_owned())
        .spawn({
            let gate = Arc::clone(&gate);
            move || super::accept::accept_loop(listener, gate)
        })
        .unwrap();
    // The loop waited instead of spinning: the hook fires only in
    // `wait_for_room`. With the comparison flipped the loop never waits, so
    // this fails at the deadline on the mutant.
    wait_until_accept_waits();
    close_within(opened.session, opened.log);
    worker.join().unwrap();
}

fn status(name: &str) -> Event {
    Event::SessionStatus(SessionStatus {
        name: name.to_owned(),
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        thinking: None,
        state: SessionState::Idle,
        since: 0,
        started: 0,
        calls: 0,
        turns: Some(0),
        step: None,
        git: None,
        context: None,
        spend: usage(),
        delegates: 0,
        jobs: 0,
        project: "-w".into(),
        clients: 0,
        closing: false,
        supervised: false,
    })
}

#[test]
fn a_full_subscribers_latest_status_survives_a_queue_saturated_after_registration() {
    reset();
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), timed).unwrap());
    log.append(&status("latest"), None, None).unwrap();
    // Registration, as `subscribe` does it, then the queue saturates and lags
    // before the latest status is delivered.
    let watcher = log.watch_all_seeded();
    let injector = watcher.injector();
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    log.append(&step(), None, None).unwrap();
    let held = Held::new(Hold::Go);
    let write = HeldWrite {
        held: Arc::clone(&held),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(write), false, mpsc::channel().1, None);
        if let Ok(()) = tx.send(()) {}
    });
    let text = held.wait_text(
        |text| text.contains("step_started"),
        "catch-up returns the durable line the saturated queue dropped",
    );
    let lines = lines_of(&text);
    let status = lines
        .iter()
        .find(|line| line["kind"] == "session_status")
        .expect("the latest status reaches a subscriber whose queue saturated");
    assert_eq!(status["payload"]["name"], "latest");
    drop(log);
    injector.push_kept(crate::client::control_line(&id, crate::client::STOP, 0));
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the writer ends once the log is dropped and it is told to stop");
}

/// A home whose `run/<id>` socket path fits no platform: longer than either
/// `SOCKET_PATH_MAX`.
fn overlong_home(temp: &fakes::TempDir) -> std::path::PathBuf {
    temp.path().join("h".repeat(200))
}

#[test]
fn resume_keeps_the_session_directory_when_the_socket_cannot_bind() {
    let temp = fakes::TempDir::new("fd");
    let home = overlong_home(&temp);
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(dir.join("events.jsonl").is_file());
    // Nothing was written: the log holds no lines.
    assert!(log::read(&dir).unwrap().is_empty());
}

#[test]
fn open_deletes_a_new_session_directory_when_the_socket_cannot_bind() {
    let temp = fakes::TempDir::new("fd");
    let home = overlong_home(&temp);
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(!dir.exists());
}

#[test]
fn resume_replaces_a_stale_socket_file() {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    let socket = home.join("run").join(&id.0);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::write(&socket, "stale").unwrap();

    let timed: Arc<dyn Clock> = clock;
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();

    UnixStream::connect(&socket).unwrap();
    close_within(session, log);
}

#[test]
fn open_refuses_a_socket_a_live_session_holds() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    // The same id in another project: its own new directory, the shared
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened.home.clone();
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket a live session holds binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::SessionHeld);
    // Nothing the live session owns was removed: its socket still
    // answers. Only the refused session's own directory is cleaned up.
    UnixStream::connect(&socket).unwrap();
    assert!(!dir.exists());
    close_within(opened.session, opened.log);
}

/// Connecting to a socket whose mode is 0o000 fails with permission denied.
/// Root connects anyway, so a run as root fails here and says why instead of
/// failing later on an unrelated assertion.
fn assert_connect_denied(socket: &std::path::Path) {
    match UnixStream::connect(socket) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        other => panic!(
            "these tests need a user other than root: connecting to a mode-0 socket gave {:?}",
            other.map(|_| ())
        ),
    }
}

#[test]
fn open_leaves_a_live_socket_it_cannot_connect_to() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    // A live listener whose mode hides it: connect fails, but a session is
    // still behind the path, so `open` must fail and leave the path alone.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o000)).unwrap();
    assert_connect_denied(&socket);
    // The same id in another project: its own new directory, the hidden
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened.home.clone();
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket a live session holds binds"),
        Err(error) => error,
    };
    // Restored before any assert that can fail, so cleanup still unlinks it.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains(&socket.display().to_string()),
        "the failure names the socket path"
    );
    assert!(
        fs::symlink_metadata(&socket).is_ok(),
        "a connect error that may hide a live session leaves its path in place"
    );
    // The live session still answers, and only the refused open's own
    // directory is cleaned up.
    UnixStream::connect(&socket).unwrap();
    assert!(!dir.exists());
    close_within(opened.session, opened.log);
}

#[test]
fn open_leaves_a_symlink_to_a_live_socket_it_cannot_connect_to() {
    reset();
    let opened = Opened::open(Vec::new());
    let live = opened.socket.clone();
    fs::set_permissions(&live, fs::Permissions::from_mode(0o000)).unwrap();
    assert_connect_denied(&live);
    // Another session's path is a symlink to the hidden live socket: it is
    // no regular file, so nothing proves it stale.
    let home = opened.home.clone();
    let id = contract::SessionId(crate::mint("s_"));
    let link = home.join("run").join(&id.0);
    std::os::unix::fs::symlink(&live, &link).unwrap();
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a symlink to a hidden live socket was replaced"),
        Err(error) => error,
    };
    // Restored before any assert that can fail, so cleanup still unlinks it.
    fs::set_permissions(&live, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()),
        "the symlink stays in place"
    );
    UnixStream::connect(&live).unwrap();
    fs::remove_file(&link).unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn close_after_resume_keeps_a_session_that_has_turns() {
    use contract::events::{InputItem, TurnStarted};
    use contract::shapes::{ContentPart, Origin, Sender};

    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: "one".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_1".into())),
                },
                changed_by: None,
            }],
        }),
        Some(contract::TurnId("t_1".into())),
        None,
    )
    .unwrap();

    let timed: Arc<dyn Clock> = clock;
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();
    close_within(session, log);

    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["turn_started"]
    );
}

#[test]
fn a_clock_move_delivers_cancelled_and_nothing_before() {
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (entered, entered_rx) = mpsc::channel();
    thread::spawn(move || {
        Deadline::after(DEADLINE)
            .recv(&entered_rx)
            .expect("the receiver is blocked");
        // No code under test waits on the clock here: the session blocks on its inbox, woken by the clock subscription rather than parked in wait_until, and the move fires the wake under test (ruling 6179406086).
        clock.advance(Duration::from_millis(1));
    });
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "nothing arrives before the clock moves"
            );
            entered.send(()).unwrap();
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("a clock move wakes the inbox");
            assert!(matches!(delivery, Delivery::Cancelled));
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// Longer than [`DEADLINE`], so a shell that misses its cancel holds `close`
/// past the test's own wait.
const SHELL_LIMIT: Duration = Duration::from_secs(30);

struct LateFlag {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl Wake for LateFlag {
    fn wake(&self) {
        *lock(&self.ready) = true;
        self.cv.notify_all();
    }
}

struct LateShell {
    saw: Arc<AtomicBool>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl Tool for LateShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(LateFlag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        let cancelled = cancel.is_cancelled();
        self.saw.store(cancelled, Ordering::Relaxed);
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancelled {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled());
        }
        Output {
            content: vec![ContentPart::Text {
                text: "stopped".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }
}

#[test]
fn a_shell_registered_after_close_is_cancelled() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let saw = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel();
    opened.session.shell(Arc::new(LateShell {
        saw: Arc::clone(&saw),
        entered: Mutex::new(Some(entered_tx)),
    }));
    let (hold, quota_entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    // The `shell` line is in the reader's buffer, behind the held `quota`:
    // the readers are stopped before it is read.
    client
        .send(&format!(
            "{}\n{}",
            r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#,
            r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#
        ))
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&quota_entered)
        .expect("the reader is inside quota");
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    assert!(
        clock.await_parked(until, fakes::Deadline::after(DEADLINE)),
        "close is past its first wait for the shells and the stop of the readers"
    );
    hold.release();
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the shell registered after close");
    assert!(saw.load(Ordering::Relaxed), "the tool saw its cancel");
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returned without the tool's timeout");
    let lines = read_to_eof(&client);
    assert_eq!(
        acks_for(&lines, "c_q").len(),
        1,
        "the quota is answered before end of file: {lines:?}"
    );
    assert_eq!(
        acks_for(&lines, "c_shell").len(),
        1,
        "the shell is answered before end of file: {lines:?}"
    );
}

/// A driver shell that answers at once: when the cancel token already
/// fired it reports the stop before start exactly as the shell tool does
/// (its text, no error, no process), and otherwise a finished command.
/// The shell thread waits for the test's release before `finish`, so the
/// test's stop lands before `Tool::run`: the pause point before the run.
struct StopFirstShell;

impl Tool for StopFirstShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        if cancel.is_cancelled() {
            return Output {
                content: vec![ContentPart::Text {
                    text: "Cancelled before it started.\n".to_owned(),
                }],
                ..Output::default()
            };
        }
        Output {
            content: vec![ContentPart::Text {
                text: "done\n".to_owned(),
            }],
            process: Some(Process {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }
}

/// A driver shell whose command really cannot start: the tool's own
/// failure, with its error and no process.
struct UnstartableShell;

impl Tool for UnstartableShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "nope\n".to_owned(),
            }],
            error: Some(contract::shapes::Failure {
                code: ErrorCode::InvalidArguments,
                message: "nope.".to_owned(),
                retry_after_ms: None,
                provider: None,
            }),
            ..Output::default()
        }
    }
}

/// Runs one driver shell through `finish` on a thread of its own, as the
/// reader does, and answers what the stop before its start says: the
/// thread pauses before `finish`, the test stops the shell, then releases
/// it, so `stop` lands before `Tool::run`.
fn stopped_before_start_answer(
    stop: impl FnOnce(&Gate),
    tool: Arc<dyn Tool>,
) -> contract::inbox::Answer {
    let opened = Opened::open(Vec::new());
    opened.session.shell(tool);
    let gate = Arc::clone(&opened.session.gate);
    let thread_gate = Arc::clone(&gate);
    let args = contract::commands::Shell {
        command: "true".to_owned(),
        send: false,
    };
    let running = crate::shell::start(&gate, &args).expect("the driver shell is wired");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let (answer_tx, answer_rx) = mpsc::channel();
    thread::spawn(move || {
        ready_tx
            .send(())
            .expect("the test waits for the pause point");
        go_rx.recv().expect("the test releases the shell");
        running.finish(
            &thread_gate,
            contract::inbox::Ack(Box::new(move |answer| {
                answer_tx
                    .send(answer)
                    .expect("the test waits for the answer");
            })),
        );
    });
    Deadline::after(DEADLINE)
        .recv(&ready_rx)
        .expect("the shell thread reached the pause point");
    stop(&gate);
    go_tx.send(()).expect("the shell thread waits for release");
    let answer = Deadline::after(DEADLINE)
        .recv(&answer_rx)
        .expect("the shell answered");
    close_within(opened.session, opened.log);
    answer
}

#[test]
fn a_shell_stopped_by_cancel_before_it_starts_is_rejected_cancelled() {
    let answer = stopped_before_start_answer(
        |gate| {
            gate.stop_running();
        },
        Arc::new(StopFirstShell),
    );
    let rejection = match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("a shell stopped before it starts is rejected"),
    };
    assert_eq!(
        rejection.code,
        ErrorCode::Cancelled,
        "a cancel stop is cancelled"
    );
    assert_eq!(
        rejection.message, "Cancelled before it started.",
        "a cancel stop keeps the tool's words"
    );
}

#[test]
fn a_shell_stopped_by_close_before_it_starts_is_rejected_closing() {
    let answer = stopped_before_start_answer(
        |gate| {
            gate.mark_closed();
            gate.cancel_shells();
        },
        Arc::new(StopFirstShell),
    );
    let rejection = match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("a shell stopped before it starts is rejected"),
    };
    assert_eq!(
        rejection.code,
        ErrorCode::Closing,
        "a close stop is closing"
    );
    assert_eq!(
        rejection.message,
        crate::client::ENDED,
        "a close stop answers as a running shell does"
    );
}

#[test]
fn a_shell_whose_command_cannot_start_keeps_invalid_arguments() {
    let answer = stopped_before_start_answer(|_| {}, Arc::new(UnstartableShell));
    let rejection = match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("a shell that cannot start is rejected"),
    };
    assert_eq!(
        rejection.code,
        ErrorCode::InvalidArguments,
        "a real no-process failure keeps its code"
    );
    assert_eq!(rejection.message, "nope.", "a real failure keeps its words");
}

#[test]
fn run_sends_the_jobs_ends_to_the_loops_inbox() {
    use contract::jobs::{Jobs as _, Opening, Stop};
    let opened = Opened::open(Vec::new());
    let dir = fakes::TempDir::new("fd-jobs");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    opened.session.jobs(jobs.clone());
    let job = jobs
        .open(Opening {
            tool: "shell".into(),
            description: "npm test".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: None,
        })
        .unwrap();
    let id = job.started.job_id.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            drop(job.end);
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the job's end reached the inbox");
            let Delivery::Job(notice) = delivery else {
                panic!("the end sent {delivery:?}");
            };
            assert_eq!(notice.completed.job_id, id);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// Hooks that hand the inbox `deliver_to` gives them to the test.
struct InboxHooks(Mutex<Option<mpsc::Sender<mpsc::Sender<Delivery>>>>);

impl contract::hook::Hooks for InboxHooks {
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(
        &self,
        _call: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(
        &self,
        _message: &contract::hook::BeforeMessage<'_>,
    ) -> contract::hook::BeforeMessageAnswer {
        contract::hook::BeforeMessageAnswer {
            outcome: contract::hook::BeforeMessageOutcome::Passed {
                replaced: None,
                context: Vec::new(),
            },
            notices: Vec::new(),
        }
    }

    fn session_start(
        &self,
        _start: &contract::hook::SessionStart<'_>,
    ) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn turn_start(&self, _turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_model_call(
        &self,
        _call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn turn_end(&self, _end: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        if let Some(tx) = self.0.lock().unwrap().as_ref() {
            let _sent = tx.send(inbox);
        }
    }
}

struct ReloadDoor;
impl contract::extension::ExtensionDoor for ReloadDoor {
    fn command(
        &self,
        _: &CommandId,
        _: &str,
        _: &str,
    ) -> Result<Box<dyn FnOnce() + Send>, contract::inbox::Rejection> {
        Ok(Box::new(|| {}))
    }
    fn seal(&self) {}
}

#[test]
fn reload_replaces_whole_answers_and_handles_and_wires_the_new_hooks() {
    use contract::events::{CommandInfo, SkillInfo, SkillSource};
    let opened = Opened::open(Vec::new());
    let command = |name: &str| CommandInfo {
        extension: false,
        name: name.into(),
        description: name.into(),
        argument_hint: None,
        tag: "skill".into(),
    };
    let skill = |name: &str| SkillInfo {
        name: name.into(),
        description: name.into(),
        path: format!("/{name}/SKILL.md"),
        source: SkillSource::Repository,
        extension: None,
        model_invocable: true,
        disabled: false,
        shadows: Vec::new(),
        shadowed_by: None,
    };
    let old_door: Arc<dyn contract::extension::ExtensionDoor> = Arc::new(ReloadDoor);
    let old_hooks: Arc<dyn contract::hook::Hooks> = Arc::new(InboxHooks(Mutex::new(None)));
    opened.session.commands(vec![command("old")]);
    opened.session.skills(vec![skill("old")]);
    opened.session.extensions(old_door.clone());
    opened.session.hooks(old_hooks.clone());
    assert_eq!(opened.session.gate.commands(), [command("old")]);
    assert_eq!(opened.session.gate.skills(), [skill("old")]);
    assert!(Arc::ptr_eq(&opened.session.gate.door().unwrap(), &old_door));
    let replace = opened.session.reloader();
    let gate = opened.session.gate.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let (tx, rx) = mpsc::channel();
            let hooks: Arc<dyn contract::hook::Hooks> = Arc::new(InboxHooks(Mutex::new(Some(tx))));
            let door: Arc<dyn contract::extension::ExtensionDoor> = Arc::new(ReloadDoor);
            replace(super::Replacement {
                commands: vec![command("new")],
                skills: vec![skill("new")],
                door: Some(door.clone()),
                hooks: Some(hooks.clone()),
            });
            assert_eq!(gate.commands(), [command("new")]);
            assert_eq!(gate.skills(), [skill("new")]);
            assert!(Arc::ptr_eq(&gate.door().unwrap(), &door));
            let given = Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("replacement hooks received the inbox");
            given.send(Delivery::Cancelled).unwrap();
            assert!(matches!(
                Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("new hooks delivered to the loop inbox"),
                Delivery::Cancelled
            ));
            replace(super::Replacement {
                commands: Vec::new(),
                skills: Vec::new(),
                door: None,
                hooks: None,
            });
            assert!(gate.commands().is_empty());
            assert!(gate.skills().is_empty());
            assert!(gate.door().is_none());
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn reloader_before_run_wires_the_replacement_hooks_when_run_starts() {
    let opened = Opened::open(Vec::new());
    let (tx, rx) = mpsc::channel();
    opened.session.reloader()(super::Replacement {
        commands: Vec::new(),
        skills: Vec::new(),
        door: None,
        hooks: Some(Arc::new(InboxHooks(Mutex::new(Some(tx))))),
    });
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("run handed the replacement hooks its inbox")
                .send(Delivery::Cancelled)
                .unwrap();
            assert!(matches!(
                Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("hooks reached the inbox"),
                Delivery::Cancelled
            ));
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn the_commands_replacer_swaps_the_whole_commands_answer() {
    use contract::events::CommandInfo;
    let opened = Opened::open(Vec::new());
    let command = |name: &str, tag: &str| CommandInfo {
        extension: false,
        name: name.into(),
        description: name.into(),
        argument_hint: None,
        tag: tag.into(),
    };
    opened
        .session
        .commands(vec![command("old", "skill"), command("gone", "template")]);
    assert_eq!(
        opened.session.gate.commands(),
        [command("old", "skill"), command("gone", "template")]
    );
    let replacer = opened.session.commands_replacer();
    replacer(vec![command("newer", "skill")]);
    assert_eq!(opened.session.gate.commands(), [command("newer", "skill")]);
    close_within(opened.session, opened.log);
}

#[test]
fn run_hands_the_hooks_the_loops_inbox() {
    let opened = Opened::open(Vec::new());
    let (given_tx, given_rx) = mpsc::channel();
    opened
        .session
        .hooks(Arc::new(InboxHooks(Mutex::new(Some(given_tx)))));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let given = Deadline::after(DEADLINE)
                .recv(&given_rx)
                .expect("waited for the hooks to be handed the inbox");
            given.send(Delivery::Cancelled).unwrap();
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("waited for the hooks' send to reach the loop's inbox");
            assert!(
                matches!(delivery, Delivery::Cancelled),
                "the hooks' sender feeds the loop's inbox: {delivery:?}"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// A driver shell that reports its start, then its cancel, and returns only
/// once the test releases it.
struct HeldShell {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    cancelled: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Tool for HeldShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(LateFlag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        if let Some(sender) = lock(&self.entered).take() {
            sender.send(()).expect("the test is waiting");
        }
        let guard = lock(&flag.ready);
        // The wait's guard is dropped before waiting for the test's
        // release below: the cancel path wakes through this same mutex,
        // so holding it across the release deadlocks the test inside
        // `Token::cancel` while this shell waits for the release.
        let (guard, _waited) = flag
            .cv
            .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled())
            .unwrap_or_else(PoisonError::into_inner);
        drop(guard);
        if cancel.is_cancelled()
            && let Some(sender) = lock(&self.cancelled).take()
        {
            sender.send(()).expect("the test is waiting");
        }
        if let Some(release) = lock(&self.release).take() {
            let _released = release.recv();
        }
        Output {
            content: vec![ContentPart::Text {
                text: "stopped".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }
}

/// A session with a [`HeldShell`] running one driver shell on a `full`
/// client. Returns the client, and the receivers that hear the shell's
/// cancel, and the sender that releases it.
struct Running {
    opened: Opened,
    client: Client,
    cancelled: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

/// Stops the session, then expects the loop's inbox to wake with
/// [`Delivery::Cancelled`]: what the stopper test checks mid-run. A check
/// returns what failed, and `running_shell` panics with it at the test's line.
fn stop_and_expect_cancel(
    inbox: &mpsc::Receiver<Delivery>,
    session: &Session,
) -> Result<(), &'static str> {
    (session.stopper())();
    match Deadline::after(DEADLINE).recv(inbox) {
        Ok(Delivery::Cancelled) => Ok(()),
        Ok(_) => Err("the stopper wakes the inbox with Cancelled"),
        Err(_) => Err("the stopper wakes the inbox"),
    }
}

/// No mid-run check: the shell runs until the test releases it.
fn no_check(_: &mpsc::Receiver<Delivery>, _: &Session) -> Result<(), &'static str> {
    Ok(())
}

#[track_caller]
fn running_shell(
    check: fn(&mpsc::Receiver<Delivery>, &Session) -> Result<(), &'static str>,
) -> Running {
    let opened = Opened::open(Vec::new());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let socket = opened.socket.clone();
    let mut connected = None;
    let mut failed = None;
    let session = &opened.session;
    session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            failed = match Deadline::after(DEADLINE).recv(&entered_rx) {
                Ok(()) => check(&inbox, session).err(),
                Err(_) => Some("the driver shell started"),
            };
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    if let Some(what) = failed {
        panic!("{what}");
    }
    Running {
        opened,
        client: connected.expect("the client connected"),
        cancelled,
        release,
    }
}

#[test]
fn the_stopper_cancels_a_running_driver_shell_and_wakes_the_loop() {
    let running = running_shell(stop_and_expect_cancel);
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("the driver shell saw its cancel");
    running.release.send(()).unwrap();
    drop(running.client);
    close_within(running.opened.session, running.opened.log);
}

#[test]
fn the_stopper_after_the_inbox_is_gone_still_cancels_later_shells() {
    let opened = Opened::open(Vec::new());
    (opened.session.stopper())();
    assert!(opened.session.gate.shells.sealed());
    close_within(opened.session, opened.log);
}

#[test]
fn abandon_unregisters_a_driver_shell_that_never_ran() {
    let opened = Opened::open(Vec::new());
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(None),
        cancelled: Mutex::new(None),
        release: Mutex::new(None),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let command = contract::commands::Shell {
        command: "true".to_owned(),
        send: false,
    };
    let Some(running) = crate::shell::start(&gate, &command) else {
        panic!("the driver shell passed its checks");
    };
    assert_eq!(gate.shells.running_len(), 1);
    running.abandon(&gate);
    assert!(
        gate.shells.running_len() == 0,
        "close would wait for a shell that never runs"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn quiesce_cancels_a_running_driver_shell_and_waits_for_it() {
    let running = running_shell(no_check);
    let session = running.opened.session;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    let quiescing = thread::spawn(move || {
        session.quiesce();
        done_tx.send(()).unwrap();
        session
    });
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("quiesce cancelled the driver shell");
    // The shell is cancelled but still running: quiesce is still waiting.
    assert!(gate.shells.running_len() != 0);
    assert!(
        done.try_recv().is_err(),
        "quiesce returned before the shell ended"
    );
    running.release.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("quiesce returned once the shell ended");
    let session = quiescing.join().unwrap();
    assert_eq!(gate.shells.running_len(), 0);
    drop(running.client);
    close_within(session, running.opened.log);
}

#[test]
fn close_cancels_a_running_driver_shell_and_waits_for_its_answer() {
    let running = running_shell(no_check);
    let session = running.opened.session;
    let log = running.opened.log;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    thread::spawn(move || {
        session.close(log);
        done_tx.send(()).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("close cancelled the driver shell");
    // The shell is cancelled but still running: close is still waiting,
    // before it stops accepting. A subscriber's writer would hold close
    // later anyway, as the shell's answer keeps its queue open.
    assert!(gate.shells.running_len() != 0);
    assert!(
        !gate.conns.stopped(),
        "close went on before the shell ended"
    );
    running.release.send(()).unwrap();
    // The answer is queued before the shell leaves the registry, so it
    // reaches the client before close drops the log.
    let answer = loop {
        let line = recv(&running.client);
        if line["payload"]["command_id"] == "c_shell" {
            break line;
        }
    };
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    assert_eq!(answer["payload"]["result"]["output"], "stopped");
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returned once the shell ended");
    assert_eq!(gate.shells.running_len(), 0);
}

#[test]
fn a_driver_shell_started_after_the_stopper_is_cancelled_and_waited_for() {
    let opened = Opened::open(Vec::new());
    let (entered_tx, entered) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let socket = opened.socket.clone();
    let mut connected = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            (opened.session.stopper())();
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the driver shell started");
    Deadline::after(DEADLINE)
        .recv(&cancelled)
        .expect("the driver shell started cancelled");
    // Registered although shutdown had begun, so close waits for it.
    assert!(gate.shells.running_len() != 0);
    release.send(()).unwrap();
    drop(connected);
    close_within(opened.session, opened.log);
    assert_eq!(gate.shells.running_len(), 0);
}

#[test]
fn close_waits_for_a_driver_shell_admitted_after_its_first_wait() {
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let start = clock.now();
    let (entered_tx, entered) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let (first_tx, first) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    let (waiting_tx, waiting) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| match point {
        Probe::FirstShellWaitDone => {
            if let Ok(()) = first_tx.send(()) {}
            lock(&resume_rx).recv().expect("the test resumes close");
        }
        Probe::ShellsWaiting => if let Ok(()) = waiting_tx.send(()) {},
        Probe::SubscribeSeeded
        | Probe::SubscribeAcknowledged
        | Probe::PrinterStopQueued
        | Probe::MessageLocked
        | Probe::CloseLocked
        | Probe::WriterParkedAtStop
        | Probe::WriterPastEnd => {}
    }));
    let socket = opened.socket.clone();
    let mut connected = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    let client = connected.expect("the client connected");
    let (done_tx, done) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&first)
        .expect("close's first wait saw no driver shell");
    client
        .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader admitted the driver shell");
    Deadline::after(DEADLINE)
        .recv(&cancelled)
        .expect("a shell admitted during teardown starts cancelled");
    resume.send(()).unwrap();
    // The shell's answer comes only when the tool ends, so the writer waits
    // for it until the grace cuts it. The cut answers `c_shell`
    // `indeterminate`; the second wait for the shell follows.
    clock.advance_when_parked(
        Some(start + super::GRACE),
        None,
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&waiting)
        .expect("close waits again for the shell once no reader is left");
    assert!(
        done.try_recv().is_err(),
        "close returned while the shell ran"
    );
    release.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returned once the shell ended");
    assert_eq!(gate.shells.running_len(), 0);
    drop(client);
}

/// A write to a rejected post-quiesce connection either lands before the
/// server closes it or fails with a closed-peer error. The read returning
/// end of file proves the rejection either way, so only another error is a
/// failure.
fn tolerated_late_write(result: io::Result<()>) {
    match result {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::ConnectionReset
            ) => {}
        Err(error) => panic!("the late client's send failed: {error:?}"),
    }
}

#[test]
fn after_quiesce_a_client_leaving_writes_no_clients_line() {
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    // The watcher blocks without a deadline, so its lines cross a channel
    // and the reads below carry the deadline.
    let mut watcher = opened.log.watch();
    let (forward, lines) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    let mut connected = None;
    let first = socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&first).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    // The ack precedes the connection's own `clients` line, so the test
    // reads that line before quiesce: one still unwritten would be taken
    // for a line that followed quiesce.
    let lines = fakes::within(
        "the joining client's clients line",
        Deadline::after(DEADLINE),
        move || {
            let wait = Deadline::after(DEADLINE);
            loop {
                let line = wait
                    .recv(&lines)
                    .expect("the joining client's clients line arrives");
                if line.kind == "clients" {
                    break;
                }
            }
            lines
        },
    );
    opened.session.quiesce();
    drop(connected);
    // A client attaching after quiesce is rejected: its line is never
    // read, so it gets end of file with no answer and no `clients` line.
    let late = Client::connect(&socket).unwrap();
    // The session closes a rejected post-quiesce connection before the
    // client's write, so the write itself can fail with a closed-peer
    // error: a rejected line is never read either way.
    tolerated_late_write(late.send(&subscribe_line("c_late", "full")));
    assert!(
        late.recv(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "no answer is written for a line never read"
    );
    assert!(
        late.recv(Deadline::after(DEADLINE)).is_none(),
        "a subscribe after quiesce ends the connection"
    );
    opened.log.append(&notice(), None, None).unwrap();
    let _lines = fakes::within("the notice", Deadline::after(DEADLINE), move || {
        let wait = Deadline::after(DEADLINE);
        loop {
            let line = wait.recv(&lines).expect("the notice arrives");
            assert_ne!(line.kind, "clients", "a clients line followed quiesce");
            if line.kind == "notice" {
                break;
            }
        }
        lines
    });
    drop(late);
    close_within(opened.session, opened.log);
}

#[test]
fn after_quiesce_a_client_waiting_for_the_shutdown_before_its_write_gets_no_answer() {
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let mut connected = None;
    let first = socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&first).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    opened.session.quiesce();
    drop(connected);
    // The read proves the close: a rejected post-quiesce connection gets
    // end of file with no answer, and the write after it lands or fails
    // depending on the platform, so it only checks for no panic on a
    // closed peer.
    let mut late = UnixStream::connect(&socket).unwrap();
    late.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut closed = [0u8; 64];
    match late.read(&mut closed) {
        Ok(0) => {}
        Ok(n) => panic!(
            "no answer is written for a line never read, got {:?}",
            &closed[..n]
        ),
        Err(error) => panic!(
            "the server closed the rejected post-quiesce connection before the deadline: {error:?}"
        ),
    }
    tolerated_late_write(
        late.write_all(format!("{}\n", subscribe_line("c_late", "full")).as_bytes()),
    );
    drop(late);
    close_within(opened.session, opened.log);
}

#[test]
fn serve_with_a_prompt_delivers_exactly_that_prompt_and_no_close() {
    reset();
    let opened = Opened::open(Vec::new());
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    opened
        .session
        .serve(Some("hi".to_owned()), Arc::new(|| false), |inbox| {
            seen.store(true, Ordering::Relaxed);
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the prompt arrives");
            let Delivery::Prompt(message, _) = delivery else {
                panic!("the first delivery is the prompt, got {delivery:?}");
            };
            assert_eq!(message.content.len(), 1);
            let ContentPart::Text { text } = &message.content[0] else {
                panic!("the prompt is text");
            };
            assert_eq!(text, "hi");
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "serve queues no close after its prompt"
            );
            Ok(())
        })
        .unwrap();
    assert!(
        ran.load(Ordering::Relaxed),
        "serve ran the loop with the queued prompt"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn serve_without_a_prompt_delivers_nothing_until_a_client_sends() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .session
        .serve(None, Arc::new(|| false), |inbox| {
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "serve with no prompt delivers nothing on its own"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// The prompt history of the project [`open`] puts its session in.
fn history_file(opened: &Opened) -> std::path::PathBuf {
    let home = opened.socket.parent().unwrap().parent().unwrap();
    home.join("projects/p/history.jsonl")
}

fn history_lines(file: &std::path::Path) -> Vec<Value> {
    match fs::read_to_string(file) {
        Ok(text) => lines_of(&text),
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            Vec::new()
        }
    }
}

fn send_text(client: &Client, id: &str, command: &str, text: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"{command}","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ))
        .unwrap();
}

/// The acknowledgement of command `id`, skipping any other line.
fn answer_of(client: &Client, id: &str) -> Value {
    let owned = id.to_owned();
    client
        .recv_until(fakes::Deadline::after(DEADLINE), |line| {
            matches!(
                line["kind"].as_str(),
                Some("command_accepted" | "command_rejected")
            ) && line["payload"]["command_id"] == owned
        })
        .expect("the acknowledgement arrived before the deadline")
}

/// The next delivery, which must be a prompt; returns its acknowledgement.
#[track_caller]
fn next_prompt(inbox: &mpsc::Receiver<Delivery>) -> contract::inbox::Ack {
    let delivery = Deadline::after(DEADLINE)
        .recv(inbox)
        .expect("a delivery arrives");
    let Delivery::Prompt(_, ack) = delivery else {
        panic!("expected a prompt, got {delivery:?}");
    };
    ack
}

fn busy() -> contract::inbox::Rejection {
    contract::inbox::Rejection {
        code: ErrorCode::Busy,
        message: "busy".into(),
    }
}

#[test]
fn a_served_session_appends_each_accepted_prompt_in_order() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let session_id = opened.session.gate.session_id.0.clone();
    let ts = contract::clock::wall_ms(opened.clock.wall());
    let watched = file.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "one");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            assert_eq!(
                history_lines(&watched).len(),
                1,
                "the line is written before the acceptance"
            );
            send_text(&client, "c_2", "prompt", "two");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_2")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    let lines = history_lines(&file);
    let texts: Vec<&Value> = lines
        .iter()
        .map(|line| &line["content"][0]["text"])
        .collect();
    assert_eq!(texts, ["one", "two"]);
    for line in &lines {
        assert_eq!(line["ts"], ts);
        assert_eq!(line["session_id"], session_id.as_str());
        assert_eq!(line["content"][0]["type"], "text");
        assert_eq!(line.as_object().unwrap().len(), 3);
    }
    close_within(opened.session, opened.log);
}

#[test]
fn a_served_session_appends_no_rejected_dropped_steered_or_own_prompt() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .serve(Some("own".into()), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_busy", "prompt", "rejected");
            (next_prompt(&inbox).0)(Err(busy()));
            assert_eq!(answer_of(&client, "c_busy")["kind"], "command_rejected");
            send_text(&client, "c_drop", "prompt", "dropped");
            drop(next_prompt(&inbox));
            let dropped = answer_of(&client, "c_drop");
            assert_eq!(dropped["kind"], "command_rejected");
            assert_eq!(dropped["payload"]["code"], "closing");
            send_text(&client, "c_steer", "steer", "steered");
            let Delivery::Steer(_, ack) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("the steer arrives");
            };
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_steer")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    assert_eq!(history_lines(&file), Vec::<Value>::new());
    close_within(opened.session, opened.log);
}

#[test]
fn an_ask_session_appends_no_prompt_even_one_a_client_sends() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "attached");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    assert_eq!(history_lines(&file), Vec::<Value>::new());
    close_within(opened.session, opened.log);
}

/// The next delivery, which must be a prompt, with its command id and
/// acknowledgement.
fn prompt_parts(inbox: &mpsc::Receiver<Delivery>) -> (CommandId, contract::inbox::Ack) {
    let delivery = Deadline::after(DEADLINE)
        .recv(inbox)
        .expect("a delivery arrives");
    let Delivery::Prompt(message, ack) = delivery else {
        panic!("expected a prompt, got {delivery:?}");
    };
    (
        message
            .sender
            .command_id
            .clone()
            .expect("a client command id"),
        ack,
    )
}

fn text_part(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: text.into() }]
}

/// Answers `closing` when dropped uncalled, as the door's guarded
/// acknowledgements do: dropping the wrapper answers the inner ack.
struct CallOnDrop {
    f: Option<Box<dyn FnOnce(contract::inbox::Answer) + Send>>,
}

impl Drop for CallOnDrop {
    fn drop(&mut self) {
        if let Some(f) = self.f.take() {
            f(Err(contract::inbox::Rejection {
                code: ErrorCode::Closing,
                message: "the session ended".into(),
            }));
        }
    }
}

#[test]
fn a_rewritten_prompt_records_its_replacement_before_its_acceptance() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let rewrites = opened.session.rewrites();
    opened
        .session
        .serve(None, Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "one");
            let (id, ack) = prompt_parts(&inbox);
            assert_eq!(id, CommandId("c_1".into()));
            rewrites(&id, &text_part("redacted"));
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    let lines = history_lines(&file);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0]["content"],
        serde_json::json!([{"type": "text", "text": "redacted"}])
    );
    close_within(opened.session, opened.log);
}

#[test]
fn a_rewrite_for_an_unknown_or_rejected_prompt_writes_nothing() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let rewrites = opened.session.rewrites();
    // No prompt registered: a steer's id and an unknown id change nothing.
    rewrites(&CommandId("c_steer".into()), &text_part("nope"));
    rewrites(&CommandId("c_unknown".into()), &text_part("nope"));
    opened
        .session
        .serve(None, Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            // A prompt rejected after its rewrite leaves no line.
            send_text(&client, "c_1", "prompt", "one");
            let (id, ack) = prompt_parts(&inbox);
            rewrites(&id, &text_part("redacted"));
            (ack.0)(Err(busy()));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_rejected");
            // Its id released: a later rewrite for it writes nothing.
            rewrites(&id, &text_part("late"));
            send_text(&client, "c_2", "prompt", "two");
            let (_, ack) = prompt_parts(&inbox);
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_2")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    let lines = history_lines(&file);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["content"][0]["text"], "two");
    close_within(opened.session, opened.log);
}

#[test]
fn a_dropped_prompt_leaves_no_pending_rewrite() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let rewrites = opened.session.rewrites();
    opened
        .session
        .serve(None, Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "same");
            let (id, ack) = prompt_parts(&inbox);
            drop(ack);
            let dropped = answer_of(&client, "c_1");
            assert_eq!(dropped["kind"], "command_rejected");
            assert_eq!(dropped["payload"]["code"], "closing");
            // The entry is gone: a rewrite for the dropped id writes nothing.
            rewrites(&id, &text_part("dropped"));
            send_text(&client, "c_2", "prompt", "same");
            let (_, ack) = prompt_parts(&inbox);
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_2")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    let lines = history_lines(&file);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["content"][0]["text"], "same");
    close_within(opened.session, opened.log);
}

#[test]
fn the_history_entry_is_gone_before_the_inner_ack_answers() {
    for dropped in [false, true] {
        reset();
        let opened = Opened::open(Vec::new());
        let file = opened.dir.join("history.jsonl");
        let id = CommandId("c_1".into());
        opened
            .session
            .gate
            .prompts
            .register(id.clone(), text_part("orig"));
        let (tx, rx) = mpsc::channel::<contract::inbox::Answer>();
        let inner = contract::inbox::Ack(Box::new({
            let gate = Arc::clone(&opened.session.gate);
            let mut on_drop = Some(CallOnDrop {
                f: Some(Box::new(move |answer: contract::inbox::Answer| {
                    assert!(
                        !gate.prompts.contains(&CommandId("c_1".into())),
                        "the entry is gone before the inner ack answers"
                    );
                    gate.prompts
                        .register(CommandId("c_1".into()), text_part("retry"));
                    drop(tx.send(answer));
                })),
            });
            move |answer: contract::inbox::Answer| {
                let mut taken = on_drop.take().expect("the inner ack answers once");
                let f = taken.f.take().expect("the inner ack answers once");
                drop(taken);
                f(answer);
            }
        }));
        let wrapper =
            crate::prompt_history::recorded(inner, file.clone(), &opened.session.gate, id.clone());
        // The loop's rewrite lands before the acknowledgement.
        opened.session.rewrites()(&id, &text_part("replaced"));
        if dropped {
            drop(wrapper);
            let answer = Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("the dropped wrapper answers closing");
            assert!(answer.is_err());
        } else {
            (wrapper.0)(Err(busy()));
            let answer = Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("the rejection is answered");
            assert!(answer.is_err());
        }
        // The retry's entry survived and takes a rewrite: answering it
        // records the retry's replacement.
        assert!(opened.session.gate.prompts.contains(&id));
        opened.session.rewrites()(&id, &text_part("final"));
        let (tx, rx) = mpsc::channel::<contract::inbox::Answer>();
        let retry = contract::inbox::Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        }));
        let wrapper =
            crate::prompt_history::recorded(retry, file.clone(), &opened.session.gate, id);
        (wrapper.0)(Ok(None));
        assert!(
            Deadline::after(DEADLINE)
                .recv(&rx)
                .expect("the retry is answered")
                .is_ok()
        );
        let lines: Vec<Value> = std::fs::read_to_string(&file)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["content"][0]["text"], "final");
        close_within(opened.session, opened.log);
    }
}

fn ui_status(extension: &str, status: &str) -> Event {
    Event::ExtensionUi(contract::events::ExtensionUi {
        extension: extension.to_owned(),
        ui: contract::events::Ui::Status {
            status: status.to_owned(),
        },
    })
}

#[test]
fn a_full_subscriber_sees_the_kept_ui_line_before_a_later_one() {
    // The seed and the registration sit under one lock: emit status A,
    // connect a client whose subscribe parks at the seed probe, append
    // status B through the log from the test thread, then release the probe
    // and read the client's first lines. Required: A then B. On the old path
    // (register, read `latest`, then inject) the same probe sits between the
    // snapshot read and the injection, so B is queued live before A is
    // injected and the client reads B then A.
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .log
        .append(&ui_status("fiber.test/a", "A"), None, None)
        .unwrap();
    let gate = Arc::clone(&opened.session.gate);
    let (parked_tx, parked) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point != Probe::SubscribeSeeded {
            return;
        }
        if let Ok(()) = parked_tx.send(()) {}
        Deadline::start().recv_or_fail(&lock(&release_rx), "the test releases the subscribe");
    }));
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            // The reader parks at the probe; it holds no test assertion yet.
            Deadline::after(DEADLINE)
                .recv(&parked)
                .expect("subscribe parked at its seed");
            // Appended after the watcher is registered: `append` returns, so no
            // lock is held when the probe releases.
            log.append(&ui_status("fiber.test/a", "B"), None, None)
                .unwrap();
            release_tx.send(()).unwrap();
            let ack = recv(&client);
            assert_eq!(ack["kind"], "command_accepted");
            let first = client
                .recv_until(fakes::Deadline::after(DEADLINE), |line| {
                    line["kind"] == "extension_ui"
                })
                .expect("the first extension_ui arrives");
            let second = client
                .recv_until(fakes::Deadline::after(DEADLINE), |line| {
                    line["kind"] == "extension_ui"
                })
                .expect("the second extension_ui arrives");
            assert_eq!(
                [
                    first["payload"]["status"].as_str().unwrap(),
                    second["payload"]["status"].as_str().unwrap()
                ],
                ["A", "B"]
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn full_subscriptions_name_the_opening_boundary_and_summary_does_not() {
    for count in [0usize, 1, 3] {
        reset();
        let opened = Opened::open(Vec::new());
        for _ in 0..count {
            opened
                .log
                .append(&Event::StepStarted(contract::events::Empty {}), None, None)
                .unwrap();
        }
        let socket = opened.socket.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |_| {
                let full = Client::connect(&socket).unwrap();
                full.send(&subscribe_line("c_full", "full")).unwrap();
                let answer = recv(&full);
                assert_eq!(answer["kind"], "command_accepted");
                assert_eq!(answer["payload"]["command_id"], "c_full");
                let expected = count
                    .checked_sub(1)
                    .map(|last| serde_json::json!({"last_seq": last}));
                assert_eq!(answer["payload"].get("result"), expected.as_ref());
                let fold = support::until(&full, |line| line["kind"] == "clients");
                let durable: Vec<_> = fold
                    .iter()
                    .filter_map(|line| line["seq"].as_u64())
                    .collect();
                assert_eq!(
                    durable,
                    (0..u64::try_from(count).unwrap()).collect::<Vec<_>>()
                );
                assert_eq!(
                    support::kinds(&fold),
                    std::iter::repeat_n("step_started", count)
                        .chain(["clients"])
                        .collect::<Vec<_>>()
                );
                let summary = Client::connect(&socket).unwrap();
                summary
                    .send(&subscribe_line("c_summary", "summary"))
                    .unwrap();
                let answer = recv(&summary);
                assert_eq!(answer["kind"], "command_accepted");
                assert!(answer["payload"].get("result").is_none());
                summary.send(&subscribe_line("c_raise", "full")).unwrap();
                let answer = answer_of(&summary, "c_raise");
                assert_eq!(answer["kind"], "command_accepted");
                assert_eq!(answer["payload"].get("result"), expected.as_ref());
                Ok(())
            })
            .unwrap();
        close_within(opened.session, opened.log);
    }
}

#[test]
fn a_full_subscribe_is_counted_before_its_acknowledgement() {
    // A client that reads its acknowledgement is counted in `clients`, so a
    // prompt sent once the acknowledgement arrives finds it connected.
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // Held weakly: the gate keeps the probe, and `close` must drop the log's
    // last handle to end the writer.
    let log = Arc::downgrade(&opened.log);
    let (count_tx, count) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point != Probe::SubscribeAcknowledged {
            return;
        }
        let latest = log
            .upgrade()
            .expect("the log is open")
            .latest("clients")
            .map_or(0, |line| line.payload["count"].as_u64().unwrap());
        if let Ok(()) = count_tx.send(latest) {}
    }));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let counted = Deadline::after(DEADLINE)
                .recv(&count)
                .expect("the subscribe was acknowledged");
            assert_eq!(counted, 1, "the acknowledgement preceded the count");
            let ack = recv(&client);
            assert_eq!(ack["kind"], "command_accepted");
            assert_eq!(ack["payload"]["command_id"], "c_full");
            let clients = client
                .recv_until(fakes::Deadline::after(DEADLINE), |line| {
                    line["kind"] == "clients"
                })
                .expect("the clients line follows the seed");
            assert_eq!(clients["payload"]["count"], 1);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

fn send_bare(client: &Client, id: &str, command: &str) {
    client
        .send(&format!(r#"{{"id":"{id}","command":"{command}"}}"#))
        .unwrap();
}

#[test]
fn a_rejected_command_id_may_be_sent_again() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_r", "prompt", "one");
            (next_prompt(&inbox).0)(Err(busy()));
            assert_eq!(answer_of(&client, "c_r")["payload"]["code"], "busy");
            send_text(&client, "c_r", "prompt", "one");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_r")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_repeated_subscribe_id_is_a_duplicate_not_an_invalid_argument() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let repeat = answer_of(&client, "c_sub");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_prompt_id_sent_again_while_the_first_is_unanswered_is_not_applied_twice() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "one");
            let held = next_prompt(&inbox);
            send_text(&client, "c_1", "prompt", "one");
            let repeat = answer_of(&client, "c_1");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            assert!(inbox.try_recv().is_err(), "the repeat was not dispatched");
            (held.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_malformed_line_does_not_free_an_accepted_id() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            send_bare(&client, "c_t", "tools");
            assert_eq!(answer_of(&client, "c_t")["kind"], "command_accepted");
            client
                .send(r#"{"id":"c_t","command":"tools","bogus":1}"#)
                .unwrap();
            assert_eq!(answer_of(&client, "c_t")["payload"]["code"], "malformed");
            send_bare(&client, "c_t", "tools");
            assert_eq!(
                answer_of(&client, "c_t")["payload"]["code"],
                "duplicate_command"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// A resumed session whose log holds `events`, closed; whether its
/// directory is left.
#[track_caller]
fn kept_after_close(events: Vec<Event>) -> bool {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    for event in events {
        log.append(&event, None, None).unwrap();
    }
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();
    close_within(session, log);
    dir.exists()
}

#[test]
fn close_keeps_a_session_whose_log_holds_an_offer_and_no_turn() {
    use contract::events::{OfferedItem, OfferedKind, RepositoryCodeOffered};

    let offered = Event::RepositoryCodeOffered(RepositoryCodeOffered {
        request_id: contract::RequestId("r_1".into()),
        items: vec![OfferedItem {
            files: vec![],
            kind: OfferedKind::McpServer,
            name: "db".into(),
            hash: "0".repeat(64),
            required: false,
            summary: "MCP server: db".into(),
            version: None,
            diff: None,
        }],
    });
    assert!(kept_after_close(vec![offered]));
    assert!(!kept_after_close(Vec::new()));
}

fn tool_info(name: &str, bytes: u64) -> contract::events::ToolInfo {
    contract::events::ToolInfo {
        name: name.into(),
        source: contract::events::ToolSource::Builtin,
        state: contract::events::ToolState::Full,
        bytes,
        tokens: None,
    }
}

/// The `tools` answer's names and byte sizes, in answer order.
fn tools_answer(client: &Client, id: &str) -> Vec<(String, u64)> {
    send_bare(client, id, "tools");
    let answer = answer_of(client, id);
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    answer["payload"]["result"]["tools"]
        .as_array()
        .expect("a tools list")
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_owned(),
                tool["bytes"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn named(entries: &[(&str, u64)]) -> Vec<(String, u64)> {
    entries
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), *bytes))
        .collect()
}

#[test]
fn the_declarer_changes_the_tools_answer() {
    reset();
    let opened = Opened::open(vec![tool_info("edit", 1), tool_info("write", 2)]);
    let socket = opened.socket.clone();
    let declare = opened.session.declarer();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            declare("web_search", Some(tool_info("web_search", 52)));
            assert_eq!(
                tools_answer(&client, "c_1"),
                named(&[("edit", 1), ("web_search", 52), ("write", 2)]),
                "added in name order"
            );
            declare("web_search", Some(tool_info("web_search", 60)));
            assert_eq!(
                tools_answer(&client, "c_2"),
                named(&[("edit", 1), ("web_search", 60), ("write", 2)]),
                "replaced, not added twice"
            );
            declare("web_search", None);
            assert_eq!(
                tools_answer(&client, "c_3"),
                named(&[("edit", 1), ("write", 2)]),
                "removed"
            );
            declare("absent", None);
            assert_eq!(
                tools_answer(&client, "c_4"),
                named(&[("edit", 1), ("write", 2)])
            );
            declare("zz", Some(tool_info("zz", 3)));
            assert_eq!(
                tools_answer(&client, "c_5"),
                named(&[("edit", 1), ("write", 2), ("zz", 3)]),
                "a name after every other goes last"
            );
            declare("aaa", Some(tool_info("aaa", 4)));
            assert_eq!(
                tools_answer(&client, "c_6"),
                named(&[("aaa", 4), ("edit", 1), ("write", 2), ("zz", 3)]),
                "a name before every other goes first"
            );
            declare("aaa", None);
            assert_eq!(
                tools_answer(&client, "c_7"),
                named(&[("edit", 1), ("write", 2), ("zz", 3)]),
                "removes the first entry"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// The `tools` answer's rows: name, state and bytes, in answer order.
/// (`tokens` is recomputed from the byte rate on every answer, so a test
/// pins stored tokens through the gate instead.)
fn tools_rows(client: &Client, id: &str) -> Vec<(String, ToolState, u64)> {
    send_bare(client, id, "tools");
    let answer = answer_of(client, id);
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    answer["payload"]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_owned(),
                serde_json::from_value(tool["state"].clone()).unwrap(),
                tool["bytes"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn sized_info(name: &str, bytes: u64, tokens: Option<u64>) -> contract::events::ToolInfo {
    contract::events::ToolInfo {
        name: name.into(),
        source: contract::events::ToolSource::Builtin,
        state: ToolState::Full,
        bytes,
        tokens,
    }
}

#[test]
fn the_state_setter_changes_only_the_named_rows_state() {
    reset();
    let opened = Opened::open(vec![
        sized_info("edit", 1, Some(2)),
        sized_info("write", 3, None),
    ]);
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let state = opened.session.state_setter();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            state("edit", ToolState::Deferred);
            assert_eq!(
                tools_rows(&client, "c_1"),
                [
                    ("edit".to_owned(), ToolState::Deferred, 1),
                    ("write".to_owned(), ToolState::Full, 3),
                ],
                "only the named row's state changes"
            );
            // Bytes, tokens and source are untouched in storage.
            assert_eq!(
                lock(&gate.tools)
                    .iter()
                    .map(|tool| (
                        tool.name.clone(),
                        tool.state,
                        tool.bytes,
                        tool.tokens,
                        tool.source.clone()
                    ))
                    .collect::<Vec<_>>(),
                [
                    (
                        "edit".to_owned(),
                        ToolState::Deferred,
                        1,
                        Some(2),
                        contract::events::ToolSource::Builtin
                    ),
                    (
                        "write".to_owned(),
                        ToolState::Full,
                        3,
                        None,
                        contract::events::ToolSource::Builtin
                    ),
                ]
            );
            state("edit", ToolState::Loaded);
            assert_eq!(tools_rows(&client, "c_2")[0].1, ToolState::Loaded,);
            state("absent", ToolState::Deferred);
            assert_eq!(
                tools_rows(&client, "c_3"),
                [
                    ("edit".to_owned(), ToolState::Loaded, 1),
                    ("write".to_owned(), ToolState::Full, 3),
                ],
                "an unknown name changes nothing and adds no row"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn the_state_setter_does_nothing_once_the_session_closes() {
    reset();
    let opened = Opened::open(vec![sized_info("edit", 1, Some(2))]);
    let state = opened.session.state_setter();
    let gate = Arc::clone(&opened.session.gate);
    close_within(opened.session, opened.log);
    state("edit", ToolState::Deferred);
    state("absent", ToolState::Deferred);
    assert_eq!(
        lock(&gate.tools)
            .iter()
            .map(|tool| (tool.name.clone(), tool.state))
            .collect::<Vec<_>>(),
        [("edit".to_owned(), ToolState::Full)]
    );
    drop(gate);
    // With the gate gone, a call still neither panics nor changes anything.
    state("edit", ToolState::Deferred);
}

#[test]
fn the_state_setter_does_nothing_once_close_begins() {
    reset();
    let opened = Opened::open(vec![sized_info("edit", 1, Some(2))]);
    let state = opened.session.state_setter();
    let gate = Arc::clone(&opened.session.gate);
    // Held registered so `close` parks in its first shells wait: the shells
    // are sealed (close began) while the connections are not yet stopped.
    let held = Arc::new(::support::cancel::Token::new());
    gate.shells.track(Arc::clone(&held));
    let (waiting_tx, waiting) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if matches!(point, Probe::ShellsWaiting) {
            let _ignored = waiting_tx.send(());
        }
    }));
    let session = opened.session;
    let log = opened.log;
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || {
        session.close(log);
        let _ignored = done_tx.send(());
    });
    Deadline::after(DEADLINE)
        .recv(&waiting)
        .expect("close parks waiting for the held shell");
    assert!(
        gate.sealed.load(Ordering::Relaxed),
        "close began before the connections stop"
    );
    assert!(
        !gate.conns.stopped(),
        "the connections stop only after the shells wait"
    );
    state("edit", ToolState::Deferred);
    state("absent", ToolState::Deferred);
    let rows = || {
        lock(&gate.tools)
            .iter()
            .map(|tool| (tool.name.clone(), tool.state))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(), [("edit".to_owned(), ToolState::Full)]);
    gate.shells.untrack(&held);
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returned");
    state("edit", ToolState::Deferred);
    assert_eq!(rows(), [("edit".to_owned(), ToolState::Full)]);
}

#[test]
fn the_declarer_does_nothing_once_the_session_closes() {
    reset();
    let opened = Opened::open(vec![tool_info("edit", 1)]);
    let declare = opened.session.declarer();
    let gate = Arc::clone(&opened.session.gate);
    close_within(opened.session, opened.log);
    declare("web_search", Some(tool_info("web_search", 52)));
    declare("edit", None);
    assert_eq!(
        lock(&gate.tools)
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        ["edit"]
    );
    drop(gate);
    // With the gate gone, a call still neither panics nor changes anything.
    declare("web_search", None);
}

#[test]
fn the_inbox_wake_wakes_the_loop_and_never_keeps_the_inbox_open() {
    let opened = Opened::open(Vec::new());
    let wake = opened.session.inbox_wake();
    let mut kept = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            wake.wake();
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the wake reaches the inbox");
            assert!(matches!(delivery, Delivery::Cancelled));
            kept = Some(inbox);
            Ok(())
        })
        .unwrap();
    let inbox = kept.unwrap();
    close_within(opened.session, opened.log);
    // The session is closed: waking does nothing, and the wake alone does
    // not hold the inbox open.
    wake.wake();
    assert!(
        matches!(
            Deadline::after(DEADLINE).recv(&inbox),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the inbox closes with the session"
    );
}

#[test]
fn close_keeps_a_started_rewound_session_with_no_prompt() {
    use contract::events::{FiberStarted, Rewind, SessionStarted, Variables, VariablesSource};
    use contract::shapes::Point;

    let started = || {
        Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: "/usr/bin".into(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: Some(Point {
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
                seq: contract::Seq(3),
            }),
            rewind: Some(Rewind {
                summary: None,
                note: "n".into(),
            }),
            worktree: None,
            supervised: false,
        })
    };
    let fiber = || {
        Event::FiberStarted(FiberStarted {
            version: "0.0.0".into(),
            resumed: false,
        })
    };
    assert!(
        kept_after_close(vec![started(), fiber()]),
        "a started rewound session is kept"
    );
    assert!(
        !kept_after_close(vec![started()]),
        "a rewind that never started leaves nothing behind"
    );
    let plain = || {
        let Event::SessionStarted(mut first) = started() else {
            panic!("a session_started");
        };
        first.forked_from = None;
        first.rewind = None;
        Event::SessionStarted(first)
    };
    assert!(
        !kept_after_close(vec![plain(), fiber()]),
        "a started session with no point to continue from is deleted"
    );
}

#[test]
fn print_ends_at_rewound_while_the_log_is_held() {
    use contract::events::{Rewound, SessionStarted, Variables, VariablesSource};
    use contract::shapes::Point;

    struct SharedOut(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedOut {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h").join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let watcher = log.watch();
    // Seeded after the watch starts, as the printer sees them live.
    for event in [
        Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: "/usr/bin".into(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: Some(Point {
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
                seq: contract::Seq(3),
            }),
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        Event::Rewound(Rewound {
            new_session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".into()),
            seq: contract::Seq(5),
            from_session_id: None,
            summary: None,
        }),
    ] {
        log.append(&event, None, None).unwrap();
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let (done, finished) = mpsc::channel();
    thread::Builder::new()
        .name("print-rewound".to_owned())
        .spawn({
            let out = Arc::clone(&out);
            move || {
                super::print(watcher, Box::new(SharedOut(out)));
                done.send(()).unwrap_or(());
            }
        })
        .unwrap();
    // The log stays held: without `rewound` closing the copy, the print
    // would wait for a `fiber_exited` that never comes.
    let _held = log;
    assert!(
        Deadline::after(DEADLINE).recv(&finished).is_ok(),
        "the copy ends at `rewound`"
    );
    let out = String::from_utf8_lossy(&lock(&out).clone()).into_owned();
    assert_eq!(out.lines().count(), 2, "both lines are copied");
    assert!(
        out.contains("\"rewound\""),
        "the last line copied is `rewound`"
    );
}

#[test]
fn ask_returns_the_first_prompts_rejection_as_its_failure() {
    reset();
    let opened = Opened::open(Vec::new());
    let failure = opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Err(contract::inbox::Rejection {
                code: ErrorCode::InvalidArguments,
                message: "The MCP server `fx`'s prompt `/greet` needs <who>.".into(),
            }));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Ok(())
        })
        .expect_err("the first prompt's rejection fails the ask");
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(
        failure.message,
        "The MCP server `fx`'s prompt `/greet` needs <who>.",
    );
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
    close_within(opened.session, opened.log);
}

#[test]
fn ask_whose_first_prompt_is_accepted_returns_what_run_returned() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Ok(())
        })
        .expect("an accepted first prompt keeps what run returned");
    close_within(opened.session, opened.log);

    reset();
    let opened = Opened::open(Vec::new());
    let failure = opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Err(crate::failure(ErrorCode::Busy, "busy"))
        })
        .expect_err("run's own failure wins");
    assert_eq!(failure.code, ErrorCode::Busy);
    close_within(opened.session, opened.log);
}

/// Appends one prompted turn, so `close` keeps the session directory and the
/// test can check `session.lock` afterwards.
fn keep_dir(log: &Arc<Log>) {
    use contract::events::{InputItem, TurnStarted};
    use contract::shapes::{Origin, Sender};
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: "one".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_1".into())),
                },
                changed_by: None,
            }],
        }),
        Some(contract::TurnId("t_1".into())),
        None,
    )
    .unwrap();
}

#[track_caller]
fn assert_lock_released(dir: &std::path::Path) {
    assert!(
        matches!(log::try_hold(dir), Ok(log::Hold::Held(_))),
        "close released session.lock"
    );
}

#[test]
fn close_returns_after_the_socket_path_was_removed() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    fs::remove_file(&opened.socket).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
}

#[test]
fn close_returns_when_the_socket_path_was_rebound() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    fs::remove_file(&opened.socket).unwrap();
    // Another listener answers at the path now: the wake connect reaches it,
    // not this session's accept.
    let _other = UnixListener::bind(&opened.socket).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
}

#[test]
fn a_reader_published_after_stop_is_rejected_not_leaked() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // The acceptor passed its stopped check before close began.
    assert!(!gate.conns.stopped());
    let (checked_tx, checked_rx) = mpsc::channel();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let closer = Arc::clone(&gate);
    thread::spawn(move || {
        Deadline::after(DEADLINE)
            .recv(&checked_rx)
            .expect("the acceptor checked before the stop");
        closer.conns.mark_stopped();
        closer
            .conns
            .join_clients(closer.clock.as_ref(), closer.clock.now() + super::GRACE);
        stopped_tx.send(()).unwrap_or(());
    });
    checked_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stopped_rx)
        .expect("the stop and the join land before the publish");
    // The acceptor now publishes after the stop, as the racy loop did: the
    // publication is rejected, its stream shut, its thread ended.
    let (_peer, stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let (_read, stop) = ::support::stoppable::reader(stream).unwrap();
    let shut = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shut);
    let both = crate::client::ender(shutdown, stop);
    let (id_tx, id_rx) = mpsc::channel::<u64>();
    let (exited_tx, exited_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let _received = id_rx.recv();
        exited_tx.send(()).unwrap_or(());
    });
    let handle = match gate.conns.push_reader(
        reader,
        Box::new(move || {
            flag.store(true, Ordering::SeqCst);
            both();
        }),
        Box::new(|| {}),
    ) {
        Ok(_) => panic!("a reader published after the stop is rejected"),
        Err(handle) => handle,
    };
    assert!(
        shut.load(Ordering::SeqCst),
        "the rejection shuts the stream"
    );
    assert!(
        gate.conns.live_len() == 0,
        "the rejected reader is never published"
    );
    drop(id_tx);
    Deadline::after(DEADLINE)
        .recv(&exited_rx)
        .expect("the rejected reader's thread ends");
    match handle.join() {
        Ok(()) | Err(_) => {}
    }
    close_within(opened.session, opened.log);
}

#[test]
fn close_connects_nowhere_through_the_socket_path() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    let socket = opened.socket.clone();
    fs::remove_file(&socket).unwrap();
    let rebound = UnixListener::bind(&socket).unwrap();
    rebound.set_nonblocking(true).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
    match rebound.accept() {
        Ok(_) => panic!("close connected through the rebound socket path"),
        Err(error) => assert_eq!(
            error.kind(),
            std::io::ErrorKind::WouldBlock,
            "the rebound listener stays quiet"
        ),
    }
}

#[test]
fn close_with_nothing_written_and_the_log_held_returns() {
    // A signal while the session process is armed, before `fiber_started`,
    // is recorded and closes with nothing written, while another `Arc<Log>`
    // (the jobs' emit) still holds the log: the printer's watcher never sees
    // the log dropped, so `close` must still return (#830).
    reset();
    let opened = Opened::open(Vec::new());
    let _held = Arc::clone(&opened.log);
    close_within(opened.session, opened.log);
}

#[test]
fn close_prints_durable_lines_dropped_before_stop() {
    // The `STOP` `close` pushes is kept ahead of the catch-up, so ending on
    // it would truncate stdout before the durable lines the queue dropped
    // are recovered, including `fiber_exited` (#830).
    reset();
    // Every write waits for the test's release, so `close`'s `STOP` is
    // queued while the printer still holds undrained lines: without the
    // drain it ends on `STOP` before the catch-up, with it every line
    // below still prints.
    struct GatedOut {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        buf: Arc<Mutex<Vec<u8>>>,
    }
    impl Write for GatedOut {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if let Ok(()) = self.entered.send(()) {}
            // Held until the test releases this write; the waits below
            // name a hang through their deadlines instead of blocking
            // forever.
            if self.release.recv().is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the test went away",
                ));
            }
            lock(&self.buf).extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let dir = sessions.join(&id.0);
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id, Arc::clone(&timed)).unwrap());
    let buf = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let out = GatedOut {
        entered: entered_tx,
        release: release_rx,
        buf: Arc::clone(&buf),
    };
    let timed: Arc<dyn Clock> = clock;
    let session = Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(out)).unwrap();
    // Observed before the first release below: `close` queues its `STOP`
    // while the printer still holds undrained lines.
    let gate = Arc::clone(&session.gate);
    let (stop_tx, stop_rx) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::PrinterStopQueued
            && let Ok(()) = stop_tx.send(())
        {}
    }));
    // Prompted, so `close` keeps the session directory: the drain's
    // catch-up re-reads the dropped lines from `events.jsonl`.
    keep_dir(&log);
    // One more durable line than the watcher's queue holds, so some are
    // dropped while the printer is held on its first write.
    const LINES: usize = 1_202;
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the printer blocks in its first write");
    for _ in 1..LINES - 1 {
        log.append(&step(), None, None).unwrap();
    }
    log.append(&exited(), None, None).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let closing = Arc::clone(&log);
    thread::spawn(move || {
        session.close(closing);
        if let Ok(()) = done_tx.send(()) {}
    });
    drop(log);
    // Forced interleaving: `close` has queued its `STOP` before the printer
    // is released, so the catch-up the drain recovers is really behind it.
    Deadline::after(DEADLINE)
        .recv(&stop_rx)
        .expect("close queues its STOP while the printer is held");
    // One deadline for the whole wait: the release loop and the completion
    // receive below share it. Releases each write in turn until every line
    // is through or the printer went away early; without the drain it ends
    // on `STOP` with most lines still undelivered.
    let wait = Deadline::after(DEADLINE);
    let mut released = 0;
    if release_tx.send(()).is_err() {
        panic!("the printer is still held on its first write");
    }
    released += 1;
    while released < LINES {
        if done_rx.try_recv().is_ok() {
            break;
        }
        match wait.recv(&entered_rx) {
            Ok(()) => {
                if release_tx.send(()).is_err() {
                    break;
                }
                released += 1;
            }
            Err(_) => break,
        }
    }
    wait.recv(&done_rx)
        .expect("close returns after the writer is released");
    let text = String::from_utf8(lock(&buf).clone()).expect("stdout is UTF-8");
    let lines = lines_of(&text);
    let seqs: Vec<u64> = lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect();
    let expected: Vec<u64> = (0..LINES as u64).collect();
    assert_eq!(
        seqs, expected,
        "stdout holds every durable line in order, including fiber_exited"
    );
    assert_eq!(
        lines.last().expect("stdout holds fiber_exited")["kind"],
        "fiber_exited"
    );
}

/// A session message and a close racing for the admission lock, in both
/// orders: the lock decides which the inbox yields first, and a `Close`
/// queued before any client admits refuses a message.
#[test]
fn message_and_close_race_in_both_lock_orders() {
    // (i) The message holds the lock while the close blocks in it.
    {
        reset();
        let opened = Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel::<()>();
        let release = Mutex::new(release);
        let (closed_tx, closed) = mpsc::channel();
        *super::lock(&gate.probe) = Some(Arc::new(move |point| match point {
            Probe::MessageLocked => {
                if let Ok(()) = entered_tx.send(()) {}
                released(&release, "the message");
            }
            Probe::CloseLocked => if let Ok(()) = closed_tx.send(()) {},
            Probe::FirstShellWaitDone
            | Probe::PrinterStopQueued
            | Probe::ShellsWaiting
            | Probe::SubscribeSeeded
            | Probe::SubscribeAcknowledged
            | Probe::WriterParkedAtStop
            | Probe::WriterPastEnd => {}
        }));
        let socket = opened.socket.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), |inbox| {
                let first = Client::connect(&socket).unwrap();
                first.send(&subscribe_line("c_sub", "full")).unwrap();
                let _ack = recv(&first);
                first
                    .send(
                        r#"{"id":"c_m1","command":"message","args":{"from_session_id":"s_1","text":"race"}}"#,
                    )
                    .unwrap();
                Deadline::after(DEADLINE)
                    .recv(&entered)
                    .expect("the message holds the lock");
                let second = Client::connect(&socket).unwrap();
                second.send(&subscribe_line("c_two", "full")).unwrap();
                let _ack = recv(&second);
                second.send(r#"{"id":"c_close","command":"close"}"#).unwrap();
                release_tx.send(()).unwrap();
                // The message was admitted under the lock the close waited
                // for: it reaches the inbox ahead of the close, still
                // unanswered, and the take's ack answers it.
                let Delivery::Message(message, ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("the message is delivered")
                else {
                    panic!("the message is delivered as a message");
                };
                assert_eq!(message.command_id.0, "c_m1");
                // The take proves admission finished: read up to a second
                // subscribe's acknowledgement and prove the message itself
                // is still unanswered. The reader answers lines in order,
                // so the acknowledgement arrives after anything admission
                // sent; the take's ack is still uncalled, so the round
                // trip orders the check instead of a wall-clock wait.
                first.send(&subscribe_line("c_probe", "full")).unwrap();
                for line in support::until(&first, |line| {
                    support::command_id(line) == Some("c_probe")
                }) {
                    assert_ne!(
                        support::command_id(&line),
                        Some("c_m1"),
                        "the door answers nothing at admission"
                    );
                }
                ack.0(Ok(None));
                let accepted = support::response(&first, "c_m1");
                assert_eq!(support::kind(&accepted), "command_accepted");
                Deadline::after(DEADLINE)
                    .recv(&closed)
                    .expect("the close took the lock next");
                let Delivery::Close(ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("the close is delivered")
                else {
                    panic!("the close is delivered as a close");
                };
                ack.0(Ok(None));
                let closed = support::response(&second, "c_close");
                assert_eq!(support::kind(&closed), "command_accepted");
                Ok(())
            })
            .unwrap();
        opened.close();
    }
    // (ii) The close holds the lock while the message blocks in it.
    {
        reset();
        let opened = Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel::<()>();
        let release = Mutex::new(release);
        *super::lock(&gate.probe) = Some(Arc::new(move |point| match point {
            Probe::CloseLocked => {
                if let Ok(()) = entered_tx.send(()) {}
                released(&release, "the close");
            }
            Probe::MessageLocked
            | Probe::FirstShellWaitDone
            | Probe::PrinterStopQueued
            | Probe::ShellsWaiting
            | Probe::SubscribeSeeded
            | Probe::SubscribeAcknowledged
            | Probe::WriterParkedAtStop
            | Probe::WriterPastEnd => {}
        }));
        let socket = opened.socket.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), |inbox| {
                let first = Client::connect(&socket).unwrap();
                first.send(&subscribe_line("c_sub", "full")).unwrap();
                let _ack = recv(&first);
                first.send(r#"{"id":"c_close","command":"close"}"#).unwrap();
                Deadline::after(DEADLINE)
                    .recv(&entered)
                    .expect("the close holds the lock");
                let second = Client::connect(&socket).unwrap();
                second.send(&subscribe_line("c_two", "full")).unwrap();
                let _ack = recv(&second);
                second
                    .send(
                        r#"{"id":"c_m1","command":"message","args":{"from_session_id":"s_1","text":"race"}}"#,
                    )
                    .unwrap();
                release_tx.send(()).unwrap();
                // The close closed admission under the lock the message
                // waited for: the message is refused, and only the close
                // reaches the inbox.
                let refused = support::response(&second, "c_m1");
                assert_eq!(support::kind(&refused), "command_rejected");
                assert_eq!(
                    support::rejection(&refused),
                    (
                        "closing",
                        "The session is closing and takes no session message."
                    )
                );
                let Delivery::Close(ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("the close is delivered")
                else {
                    panic!("the close is delivered as a close");
                };
                ack.0(Ok(None));
                let closed = support::response(&first, "c_close");
                assert_eq!(support::kind(&closed), "command_accepted");
                assert!(
                    inbox.try_recv().is_err(),
                    "the refused message reached no inbox"
                );
                Ok(())
            })
            .unwrap();
        opened.close();
    }
    // (iii) A `Close` queued before any client admits refuses a message.
    {
        reset();
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        let (taken_tx, taken) = mpsc::channel();
        opened
            .session
            .run(
                vec![Delivery::Close(contract::inbox::Ack(Box::new(move |_| {})))],
                Arc::new(|| false),
                |inbox| {
                    taken_tx.send(()).unwrap();
                    let first = Client::connect(&socket).unwrap();
                    first.send(&subscribe_line("c_sub", "full")).unwrap();
                    let _ack = recv(&first);
                    first
                        .send(
                            r#"{"id":"c_m1","command":"message","args":{"from_session_id":"s_1","text":"race"}}"#,
                        )
                        .unwrap();
                    let refused = support::response(&first, "c_m1");
                    assert_eq!(support::kind(&refused), "command_rejected");
                    assert_eq!(
                        support::rejection(&refused),
                        (
                            "closing",
                            "The session is closing and takes no session message."
                        )
                    );
                    assert!(
                        matches!(inbox.try_recv(), Ok(Delivery::Close(_))),
                        "only the queued close is waiting"
                    );
                    Ok(())
                },
            )
            .unwrap();
        Deadline::after(DEADLINE)
            .recv(&taken)
            .expect("the run closure ran");
        opened.close();
    }
}

/// A `subscribe` line asking for `label`, as `fiber resume --credential`
/// sends it through the hub.
fn subscribe_credential_line(id: &str, level: &str, label: &str) -> String {
    format!(
        r#"{{"id":"{id}","command":"subscribe","args":{{"level":"{level}","credential":"{label}"}}}}"#
    )
}

/// A switch refusal naming an unknown label, as the credential resolver
/// words one.
fn unknown_label(label: &str) -> contract::inbox::Rejection {
    contract::inbox::Rejection {
        code: ErrorCode::CredentialMissing,
        message: format!("no credential named `{label}`"),
    }
}

/// The next delivery, which must be a credential switch, with its label
/// and acknowledgement.
#[track_caller]
fn next_switch(inbox: &mpsc::Receiver<Delivery>) -> (String, contract::inbox::Ack) {
    let delivery = Deadline::after(DEADLINE)
        .recv(inbox)
        .expect("a delivery arrives");
    let Delivery::Credential(args, ack) = delivery else {
        panic!("expected a credential switch, got {delivery:?}");
    };
    (args.label, ack)
}

#[test]
fn a_first_subscribe_with_a_label_waits_for_the_switch_before_its_acceptance() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .log
        .append(&Event::StepStarted(contract::events::Empty {}), None, None)
        .unwrap();
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "full", "work"))
                .unwrap();
            // The switch reaches the loop with the open, before any later
            // prompt could: nothing else is delivered with it.
            let (label, ack) = next_switch(&inbox);
            assert_eq!(label, "work");
            assert!(
                inbox.try_recv().is_err(),
                "no other command is delivered with the switch"
            );
            // The writer is parked at the switch's response wait: it
            // cannot emit the acceptance or folded history until the
            // test answers. The socket is polled without waiting, so no
            // line hides behind a delay and no clock is read; anything
            // already written is a premature line and fails here, and
            // the ordered read below would fail on it too.
            wait_switch_waiting();
            assert!(
                client
                    .recv(fakes::Deadline::after(Duration::ZERO))
                    .is_none(),
                "no line precedes the loop's answer"
            );
            (ack.0)(Ok(None));
            // The acceptance is the connection's first line: nothing
            // preceded it, and it names the last opening line.
            let accepted = recv(&client);
            assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
            assert_eq!(accepted["payload"]["command_id"], "c_sub", "{accepted}");
            assert_eq!(accepted["payload"]["result"]["last_seq"], 0, "{accepted}");
            // Folded history follows the acceptance, never precedes it:
            // the seeded step comes before the live `clients` count.
            let lines = support::until(&client, |line| line["kind"] == "clients");
            assert_eq!(support::kinds(&lines), ["step_started", "clients"]);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_first_subscribe_with_an_unknown_label_is_accepted_then_noticed() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .log
        .append(&Event::StepStarted(contract::events::Empty {}), None, None)
        .unwrap();
    let socket = opened.socket.clone();
    let dir = opened.dir.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "full", "nope"))
                .unwrap();
            let (label, ack) = next_switch(&inbox);
            assert_eq!(label, "nope");
            (ack.0)(Err(unknown_label("nope")));
            // The subscribe itself still succeeds: one acceptance,
            // then the refusal as an ephemeral notice, never a rejection
            // of the subscribe id.
            let accepted = answer_of(&client, "c_sub");
            assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
            assert_eq!(accepted["payload"]["result"]["last_seq"], 0, "{accepted}");
            let notice = recv(&client);
            assert_eq!(notice["kind"], "notice", "{notice}");
            assert_eq!(notice["payload"]["code"], "credential_missing", "{notice}");
            assert_eq!(
                notice["payload"]["message"], "no credential named `nope`",
                "{notice}"
            );
            assert!(
                notice["payload"].get("extension").is_none(),
                "no new payload key: {notice}"
            );
            let history = support::until(&client, |line| line["kind"] == "clients");
            assert_eq!(support::kinds(&history), ["step_started", "clients"]);
            // The successful subscription stands: the next prompt is
            // delivered and answered as usual, on the old label.
            send_text(&client, "c_1", "prompt", "hi");
            let Delivery::Prompt(_, ack) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("the prompt after a refused switch is delivered");
            };
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            assert!(
                inbox.try_recv().is_err(),
                "no second command was created for the switch"
            );
            Ok(())
        })
        .unwrap();
    // The notice is connection-local and ephemeral: the log holds none.
    let text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(
        text.lines().all(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|line| line.get("kind").cloned())
                .and_then(|kind| kind.as_str().map(str::to_owned))
                != Some("notice".to_owned())
        }),
        "no durable notice"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn a_summary_subscribe_with_a_failing_label_notices_before_its_snapshots() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            log.append(&status("s1"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "summary", "nope"))
                .unwrap();
            let (_, ack) = next_switch(&inbox);
            (ack.0)(Err(unknown_label("nope")));
            let mut lines = Vec::new();
            for _ in 0..4 {
                lines.push(recv(&client));
            }
            let kinds: Vec<&str> = lines
                .iter()
                .map(|line| line["kind"].as_str().unwrap())
                .collect();
            assert_eq!(
                kinds,
                [
                    "command_accepted",
                    "notice",
                    "session_status",
                    "extensions_loaded",
                ],
                "the notice reaches even a summary connection, before its snapshots"
            );
            assert_eq!(lines[0]["payload"]["command_id"], "c_sub");
            assert_eq!(lines[1]["payload"]["code"], "credential_missing");
            assert_eq!(lines[2]["payload"]["name"], "s1");
            // Ordinary summary filtering is unchanged: a later durable
            // line that is not status or extensions never reaches this
            // client, while a later status does. Both are appended before
            // reading, so the status arriving proves the step was already
            // consumed and skipped: no clock is read.
            log.append(&step(), None, None).unwrap();
            log.append(&status("s2"), None, None).unwrap();
            let lines = support::until(&client, |line| {
                line["kind"] == "session_status" && line["payload"]["name"] == "s2"
            });
            assert_eq!(
                support::kinds(&lines),
                ["session_status"],
                "a summary connection still skips other durable lines"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_duplicate_label_bearing_subscribe_sends_no_second_switch() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_1", "full", "work"))
                .unwrap();
            let (_, ack) = next_switch(&inbox);
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            client
                .send(&subscribe_credential_line("c_1", "full", "work"))
                .unwrap();
            let rejected = answer_of(&client, "c_1");
            assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
            assert_eq!(
                rejected["payload"]["code"], "duplicate_command",
                "{rejected}"
            );
            assert!(
                inbox.try_recv().is_err(),
                "a duplicate id is never delivered twice"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_same_level_label_bearing_subscribe_sends_no_switch() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_1", "full")).unwrap();
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            client
                .send(&subscribe_credential_line("c_2", "full", "work"))
                .unwrap();
            let rejected = answer_of(&client, "c_2");
            assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
            assert_eq!(
                rejected["payload"]["code"], "invalid_arguments",
                "{rejected}"
            );
            assert!(
                inbox.try_recv().is_err(),
                "a refused level change delivers no switch"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_closing_label_bearing_subscribe_sends_no_switch() {
    reset();
    let opened = Opened::open(Vec::new());
    let Opened {
        session,
        log,
        socket,
        sessions,
        id,
        clock,
        ..
    } = opened;
    session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            // The session holds only a weak log: dropping the last Arc
            // closes preparation with `closing`, before any switch.
            drop(log);
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_1", "full", "work"))
                .unwrap();
            let rejected = answer_of(&client, "c_1");
            assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
            assert_eq!(rejected["payload"]["code"], "closing", "{rejected}");
            assert!(
                inbox.try_recv().is_err(),
                "a refused subscription delivers no switch"
            );
            Ok(())
        })
        .unwrap();
    // The log is reopened only to close the session; the refusal above
    // ran while no Arc remained.
    let log = Arc::new(Log::open(&sessions, id, clock).unwrap());
    close_within(session, log);
}

#[test]
fn a_closing_session_cancels_a_pending_label_switch() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "full", "work"))
                .unwrap();
            let (label, ack) = next_switch(&inbox);
            assert_eq!(label, "work");
            // The loop never answers: it holds the acknowledgement past
            // the close, as a loop mid-turn does. The close still ends
            // the waiting writer instead of hanging its join.
            std::mem::forget(ack);
            drop(client);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_closing_session_cancels_a_pending_label_switch_despite_a_held_acknowledgement() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let (held_tx, held_rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "full", "work"))
                .unwrap();
            let (label, ack) = next_switch(&inbox);
            assert_eq!(label, "work");
            // The loop never answers: it holds the acknowledgement past
            // the close, as a loop mid-turn does.
            std::mem::forget(ack);
            // This prompt's acknowledgement clones the outbox, and the
            // test holds it past the disconnect: the settle list survives
            // the connection's own handle dropping, so only the reader's
            // clear ends the waiting writer instead of hanging its join.
            send_text(&client, "c_prompt", "prompt", "hello");
            let prompt_ack = next_prompt(&inbox);
            held_tx.send(prompt_ack).unwrap();
            drop(client);
            Ok(())
        })
        .unwrap();
    let held = Deadline::after(DEADLINE)
        .recv(&held_rx)
        .expect("the held acknowledgement arrives");
    close_within(opened.session, opened.log);
    drop(held);
}

#[test]
fn a_dead_client_ends_a_label_bearing_subscribe_without_a_hang() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let before = descriptors();
            let client = Client::connect(&socket).unwrap();
            client
                .send(&subscribe_credential_line("c_sub", "full", "work"))
                .unwrap();
            let (_, ack) = next_switch(&inbox);
            // The client is already gone: the prelude write fails and the
            // writer ends; the reader's cleanup reaps it either way.
            drop(client);
            (ack.0)(Ok(None));
            wait_released(&gate, before);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_level_change_with_a_label_waits_for_the_switch() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .log
        .append(&Event::StepStarted(contract::events::Empty {}), None, None)
        .unwrap();
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_1", "summary")).unwrap();
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            client
                .send(&subscribe_credential_line("c_2", "full", "work"))
                .unwrap();
            let (label, ack) = next_switch(&inbox);
            assert_eq!(label, "work");
            // Raising writes nothing before the loop answers: the writer
            // is parked at the switch's response wait, past the empty
            // drain, so the socket poll below sees the wait, not a
            // thread that never ran. No clock is read.
            wait_switch_waiting();
            assert!(
                client
                    .recv(fakes::Deadline::after(Duration::ZERO))
                    .is_none(),
                "no line precedes the loop's answer"
            );
            (ack.0)(Ok(None));
            // The acceptance is the next line on the wire: the drain was
            // empty and no folded history precedes the loop's answer.
            let accepted = recv(&client);
            assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
            assert_eq!(accepted["payload"]["command_id"], "c_2", "{accepted}");
            assert_eq!(accepted["payload"]["result"]["last_seq"], 0, "{accepted}");
            let first = support::until(&client, |line| line["kind"] == "clients");
            assert_eq!(support::kinds(&first), ["step_started", "clients"]);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_level_change_with_a_failing_label_notices_before_new_history() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_1", "summary")).unwrap();
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            client
                .send(&subscribe_credential_line("c_2", "full", "nope"))
                .unwrap();
            let (_, ack) = next_switch(&inbox);
            (ack.0)(Err(unknown_label("nope")));
            // One acceptance, then the notice, then folded history: the
            // drain ran before the answer, the snapshots after it.
            let accepted = answer_of(&client, "c_2");
            assert_eq!(accepted["kind"], "command_accepted", "{accepted}");
            let notice = recv(&client);
            assert_eq!(notice["kind"], "notice", "{notice}");
            assert_eq!(notice["payload"]["code"], "credential_missing", "{notice}");
            let history = support::until(&client, |line| line["kind"] == "clients");
            assert_eq!(support::kinds(&history), ["clients"]);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_second_send_on_a_driven_connection_writes_nothing() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (tx, rx) = mpsc::channel();
    let mut conn = crate::client::Conn::driven(
        gate,
        contract::inbox::Ack(Box::new(move |answer| {
            tx.send(answer).unwrap();
        })),
        "fiber.test/a",
    );
    // The at-once answer takes the driven answer; the acknowledgement held
    // for the loop finds the sink answered and writes nothing more.
    crate::client::accept(&mut conn, CommandId("c_2".into()), None);
    let ack = crate::client::inbox_ack(&mut conn, CommandId("c_1".into()));
    let answered = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the first send writes its answer");
    assert!(
        matches!(answered, Ok(None)),
        "the first send answers acceptance: {answered:?}"
    );
    // The held acknowledgement finds the answer taken and writes nothing
    // more: the answer is answered once, so the sender is consumed and the
    // channel disconnects. If the answer were still held, the sender would
    // be alive and this would see `Empty`, not `Disconnected`.
    drop(ack);
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
        "the taken answer is answered once"
    );
    close_within(opened.session, opened.log);
}

fn subscribe_line_for(id: &str) -> contract::commands::CommandLine {
    subscribe_line_for_level(id, contract::commands::SubscribeLevel::Full)
}

fn subscribe_line_for_level(
    id: &str,
    level: contract::commands::SubscribeLevel,
) -> contract::commands::CommandLine {
    contract::commands::CommandLine {
        id: CommandId(id.into()),
        command: contract::commands::Command::Subscribe(contract::commands::SubscribeArgs {
            level,
            credential: None,
        }),
    }
}

#[test]
fn dispatch_subscribe_on_driven_is_rejected_and_frees_id() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (tx, rx) = mpsc::channel();
    let id = CommandId("c_9".into());
    assert!(gate.accepted.reserve(&id));
    let mut conn = crate::client::Conn::driven(
        Arc::clone(&gate),
        contract::inbox::Ack(Box::new(move |answer| {
            tx.send(answer).unwrap();
        })),
        "fiber.test/a",
    );
    // A driven connection is already subscribed: a same-level subscribe is
    // rejected through `level::change`, and a rejection frees the id.
    let line = subscribe_line_for("c_9");
    crate::client::dispatch(&mut conn, line, "subscribe");
    let answered = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the driven subscribe is answered");
    let rejection = match answered {
        Err(rejection) => rejection,
        Ok(_) => panic!("a driven subscribe is rejected: {answered:?}"),
    };
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, crate::client::ALREADY);
    assert!(gate.accepted.reserve(&id), "a rejection frees the id");
    close_within(opened.session, opened.log);
}

#[test]
fn dispatch_subscribe_on_answered_writes_nothing_and_frees_id() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let id = CommandId("c_9".into());
    assert!(gate.accepted.reserve(&id));
    let mut conn = crate::client::Conn {
        id: 0,
        gate: Arc::clone(&gate),
        sink: crate::client::Sink::Answered {
            origin: contract::shapes::Origin::Driver,
        },
        full: true,
        gone: false,
        attribution: None,
    };
    // An answered connection has no stream and no writer: the subscribe
    // frees the id and leaves the connection alone.
    let line = subscribe_line_for("c_9");
    crate::client::dispatch(&mut conn, line, "subscribe");
    assert!(!conn.gone, "an answered subscribe marks nothing gone");
    assert!(gate.accepted.reserve(&id), "a rejection frees the id");
    close_within(opened.session, opened.log);
}

#[test]
fn dispatch_subscribe_on_driven_at_summary_marks_gone() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (tx, rx) = mpsc::channel();
    let id = CommandId("c_9".into());
    assert!(gate.accepted.reserve(&id));
    let mut conn = crate::client::Conn::driven(
        Arc::clone(&gate),
        contract::inbox::Ack(Box::new(move |answer| {
            tx.send(answer).unwrap();
        })),
        "fiber.test/a",
    );
    // A summary subscribe on a driven connection runs `level::change`, as a
    // socket's second subscribe does: with no writer to switch it marks the
    // connection gone and answers nothing at this call site.
    let line = subscribe_line_for_level("c_9", contract::commands::SubscribeLevel::Summary);
    crate::client::dispatch(&mut conn, line, "subscribe");
    assert!(
        conn.gone,
        "a driven summary subscribe marks the connection gone"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a driven summary subscribe answers nothing at this call site"
    );
    drop(conn);
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
        "the held driven answer is dropped unanswered"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn a_failed_summary_write_leaves_the_socket_connection_direct() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let id = CommandId("c_9".into());
    assert!(gate.accepted.reserve(&id));
    // Dropping the peer makes every write to `direct` fail, so the
    // acknowledgement never reaches the client.
    let (direct, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let mut conn = crate::client::Conn {
        id: 0,
        gate: Arc::clone(&gate),
        sink: crate::client::Sink::Direct {
            direct,
            writer: Box::new(Vec::<u8>::new()),
        },
        full: false,
        gone: false,
        attribution: None,
    };
    let line = subscribe_line_for_level("c_9", contract::commands::SubscribeLevel::Summary);
    crate::client::dispatch(&mut conn, line, "subscribe");
    assert!(
        conn.gone,
        "a failed acknowledgement marks the connection gone"
    );
    assert!(
        !conn.subscribed(),
        "a failed summary write leaves the socket connection direct"
    );
    close_within(opened.session, opened.log);
}

/// A closure held inside a command: it signals when entered and waits until
/// the test releases it, so a reader (or a login's thread) sits mid-dispatch
/// at `close`. Any number of entries pass once released.
struct Dispatch {
    open: Mutex<bool>,
    ready: Condvar,
    entered: Mutex<mpsc::Sender<()>>,
}

impl Dispatch {
    fn new() -> (Arc<Self>, mpsc::Receiver<()>) {
        let (entered, signal) = mpsc::channel();
        let hold = Arc::new(Self {
            open: Mutex::new(false),
            ready: Condvar::new(),
            entered: Mutex::new(entered),
        });
        (hold, signal)
    }

    /// Signals the entry, then waits for [`Dispatch::release`], bounded by
    /// the test's deadline so a missed release ends the closure too.
    fn block(&self) {
        if let Ok(()) = lock(&self.entered).send(()) {}
        let open = lock(&self.open);
        let waited = self
            .ready
            .wait_timeout_while(open, DEADLINE, |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
        drop(waited);
    }

    fn release(&self) {
        *lock(&self.open) = true;
        self.ready.notify_all();
    }
}

fn wire_quota(session: &Session, hold: &Arc<Dispatch>) {
    let hold = Arc::clone(hold);
    session.quota(Arc::new(move |_fetch| {
        hold.block();
        (Vec::new(), 80.0)
    }));
}

fn wire_login(session: &Session, hold: &Arc<Dispatch>) {
    let hold = Arc::clone(hold);
    session.mcp_login(Arc::new(move |_server, _deliver| {
        hold.block();
        Ok(())
    }));
}

/// A client subscribed `full` on the session's socket, connected while the
/// loop runs: `run` has returned and dropped its inbox when this does.
fn subscribed_client(opened: &Opened) -> Client {
    let socket = opened.socket.clone();
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "full")).unwrap();
            let _ack = recv(&client);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client subscribed")
}

/// Every line the client receives until the session shuts its connection.
fn read_to_eof(client: &Client) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.recv(Deadline::after(DEADLINE)) {
        lines.push(line);
    }
    lines
}

/// The acknowledgements `lines` holds for command `id`.
fn acks_for<'a>(lines: &'a [Value], id: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| {
            matches!(
                line["kind"].as_str(),
                Some("command_accepted" | "command_rejected")
            ) && line["payload"]["command_id"] == id
        })
        .collect()
}

/// Closes `session` on its own thread, and says when it returned.
fn close_on_thread(session: Session, log: Arc<Log>) -> (thread::ThreadId, mpsc::Receiver<()>) {
    let (done_tx, done) = mpsc::channel();
    let closer = thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    (closer.thread().id(), done)
}

/// Waits until `watcher` reaches the log's end: `close` has stopped the
/// readers and removed the socket by then, and ends the watchers next.
fn await_log_end(watcher: log::Watcher) {
    let (tx, ended) = mpsc::channel();
    thread::spawn(move || {
        let mut watcher = watcher;
        while let Ok(Some(_)) = watcher.recv() {}
        if let Ok(()) = tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&ended)
        .expect("the log's end reaches the watcher");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the quiesce waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn a_command_held_past_the_quiesce_grace_is_answered_indeterminate_once() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let start = clock.now();
    let (done_tx, done) = mpsc::channel();
    let session = opened.session;
    let quiescer = thread::spawn(move || {
        session.quiesce();
        if let Ok(()) = done_tx.send(()) {}
        session
    });
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(quiescer.thread().id()),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    // The grace passes with the command still running: one indeterminate
    // answer on the connection, before quiesce returns. Skip the `clients`
    // line the subscribe drew first.
    let answer = loop {
        let line = recv(&client);
        if matches!(
            line["kind"].as_str(),
            Some("command_accepted" | "command_rejected")
        ) {
            break line;
        }
    };
    assert_eq!(answer["kind"], "command_rejected", "{answer:?}");
    assert_eq!(answer["payload"]["command_id"], "c_q", "{answer:?}");
    assert_eq!(answer["payload"]["code"], "indeterminate", "{answer:?}");
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("quiesce returns once the cut is flushed");
    // The command ends after the cut: its own answer reaches nobody.
    hold.release();
    let session = quiescer.join().expect("the quiesce thread returns");
    close_within(session, opened.log);
}

/// `docs/invocation.md`, "Lifecycle": after a clean exit (the log ends in
/// `fiber_exited`) the session never read the command — and the exit line
/// itself still reaches a connection the quiesce cut: the cut flushes the
/// answers owed, then the writer carries on streaming instead of ending.
#[test]
fn a_connection_cut_at_the_quiesce_grace_streams_fiber_exited_before_end_of_file() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let start = clock.now();
    let (done_tx, done) = mpsc::channel();
    let session = opened.session;
    let quiescer = thread::spawn(move || {
        session.quiesce();
        if let Ok(()) = done_tx.send(()) {}
        session
    });
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(quiescer.thread().id()),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    // The grace passes with the command still running: one indeterminate
    // answer on the connection, before quiesce returns.
    let mut lines = Vec::new();
    loop {
        let line = recv(&client);
        let is_answer = matches!(
            line["kind"].as_str(),
            Some("command_accepted" | "command_rejected")
        );
        lines.push(line);
        if is_answer {
            break;
        }
    }
    let answer = lines.last().unwrap().clone();
    assert_eq!(answer["kind"], "command_rejected", "{answer:?}");
    assert_eq!(answer["payload"]["command_id"], "c_q", "{answer:?}");
    assert_eq!(answer["payload"]["code"], "indeterminate", "{answer:?}");
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("quiesce returns once the cut is flushed");
    // The exit line is written after quiesce returns, the way `run_turn`
    // writes `fiber_exited` after it: the cut connection still streams it.
    opened.log.append(&exited(), None, None).unwrap();
    hold.release();
    let session = quiescer.join().expect("the quiesce thread returns");
    close_within(session, opened.log);
    lines.extend(read_to_eof(&client));
    let acks = acks_for(&lines, "c_q");
    assert_eq!(acks.len(), 1, "one answer for the cut command: {lines:?}");
    assert_eq!(acks[0]["kind"], "command_rejected", "{lines:?}");
    let cut_at = lines.iter().position(|line| line == acks[0]).unwrap();
    assert!(
        lines[cut_at..]
            .iter()
            .any(|line| line["kind"] == "fiber_exited"),
        "fiber_exited reaches the cut connection after its answers: {lines:?}"
    );
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn a_reader_mid_dispatch_at_close_is_answered_before_end_of_file() {
    reset();
    let opened = Opened::open(Vec::new());
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let watcher = opened.log.watch();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    await_log_end(watcher);
    hold.release();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns once the answer is written");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_q");
    assert_eq!(acks.len(), 1, "one answer before end of file: {lines:?}");
    assert_eq!(acks[0]["kind"], "command_accepted", "{lines:?}");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "Lifecycle": after a clean exit (the log ends in `fiber_exited`) the session never read the command.
#[test]
fn a_reader_mid_dispatch_at_quiesce_is_answered_before_quiesce_returns() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    // Quiesce parks waiting for the owed answer: the first shells wait is
    // done, but quiesce itself cannot return while the answer is owed.
    let (parked_tx, parked) = mpsc::channel();
    *lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::FirstShellWaitDone
            && let Ok(()) = parked_tx.send(())
        {}
    }));
    let (done_tx, done) = mpsc::channel();
    let session = opened.session;
    let quiesced = thread::spawn(move || {
        session.quiesce();
        if let Ok(()) = done_tx.send(()) {}
        session
    });
    Deadline::after(DEADLINE)
        .recv(&parked)
        .expect("quiesce enters its answer phase");
    assert!(
        done.try_recv().is_err(),
        "quiesce returns before the held answer is written"
    );
    hold.release();
    // The acknowledgement is written before it is marked answered, so when
    // quiesce returns the answer is already on the connection: skip the
    // `clients` line the subscribe drew first.
    let answer = loop {
        let line = recv(&client);
        if matches!(
            line["kind"].as_str(),
            Some("command_accepted" | "command_rejected")
        ) {
            break line;
        }
    };
    assert_eq!(answer["kind"], "command_accepted", "{answer:?}");
    assert_eq!(answer["payload"]["command_id"], "c_q", "{answer:?}");
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("quiesce returns once the answer is written");
    let session = quiesced.join().expect("the quiesce thread returns");
    close_within(session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn a_dispatch_past_the_close_grace_is_answered_indeterminate_once() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let start = clock.now();
    let (closer, done) = close_on_thread(opened.session, opened.log);
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(closer),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns with the command still running");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_q");
    assert_eq!(acks.len(), 1, "one answer before end of file: {lines:?}");
    assert_eq!(acks[0]["kind"], "command_rejected", "{lines:?}");
    assert_eq!(acks[0]["payload"]["code"], "indeterminate", "{lines:?}");
    assert_eq!(
        acks[0]["payload"]["message"],
        "The session ended before answering; the command may have run."
    );
    // The command ends after the cut: its own answer reaches nobody.
    hold.release();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn an_mcp_login_released_inside_the_grace_is_answered_with_its_own_answer() {
    reset();
    let opened = Opened::open(Vec::new());
    let (hold, entered) = Dispatch::new();
    wire_login(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_login","command":"mcp_login","args":{"server":"s"}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the login is running");
    let watcher = opened.log.watch();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    await_log_end(watcher);
    hold.release();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns once the login answered");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_login");
    assert_eq!(acks.len(), 1, "one answer before end of file: {lines:?}");
    assert_eq!(acks[0]["kind"], "command_accepted", "{lines:?}");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn an_mcp_login_still_running_at_the_grace_is_answered_indeterminate() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_login(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_login","command":"mcp_login","args":{"server":"s"}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the login is running");
    let start = clock.now();
    let (closer, done) = close_on_thread(opened.session, opened.log);
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(closer),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns with the login still running");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_login");
    assert_eq!(acks.len(), 1, "one answer before end of file: {lines:?}");
    assert_eq!(acks[0]["payload"]["code"], "indeterminate", "{lines:?}");
    hold.release();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn a_malformed_rejection_reusing_an_in_flight_id_leaves_it_unanswered() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_login(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_login","command":"mcp_login","args":{"server":"s"}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the login is running");
    // The reader is free again: a line that cannot be read is rejected
    // `malformed` under the login's id, which counts as no answer for it.
    client.send(r#"{"id":"c_login","command":7}"#).unwrap();
    loop {
        let seen = recv(&client);
        if seen["kind"] == "command_rejected" {
            assert_eq!(seen["payload"]["code"], "malformed", "{seen}");
            break;
        }
    }
    let start = clock.now();
    let (closer, done) = close_on_thread(opened.session, opened.log);
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(closer),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns with the login still running");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_login");
    assert_eq!(acks.len(), 1, "the login is answered at the cut: {lines:?}");
    assert_eq!(acks[0]["payload"]["code"], "indeterminate", "{lines:?}");
    hold.release();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": a session stops reading as it ends, so a line it never read gets no answer from it.
#[test]
fn the_reader_is_stopped_before_the_socket_path_is_removed() {
    reset();
    let opened = Opened::open(Vec::new());
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let socket = opened.socket.clone();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    let gone = Deadline::after(DEADLINE);
    while socket.exists() {
        assert!(
            !gone.left().is_zero(),
            "waited until the deadline for close to remove the socket"
        );
        thread::yield_now();
    }
    hold.release();
    client
        .send(r#"{"id":"c_late","command":"tools"}"#)
        .unwrap_or(());
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns once the held command answered");
    let lines = read_to_eof(&client);
    assert_eq!(acks_for(&lines, "c_q").len(), 1, "{lines:?}");
    assert!(
        acks_for(&lines, "c_late").is_empty(),
        "a line written after the readers stopped is never read: {lines:?}"
    );
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn a_subscribe_while_closing_is_answered_and_close_returns() {
    reset();
    let opened = Opened::open(Vec::new());
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(&format!(
            "{}\n{}",
            r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#,
            r#"{"id":"c_down","command":"subscribe","args":{"level":"summary"}}"#
        ))
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let watcher = opened.log.watch();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    await_log_end(watcher);
    hold.release();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns once the subscribe was answered");
    let lines = read_to_eof(&client);
    assert_eq!(acks_for(&lines, "c_q").len(), 1, "{lines:?}");
    assert_eq!(acks_for(&lines, "c_down").len(), 1, "{lines:?}");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn a_prompt_after_the_loop_returned_is_answered_closing_before_end_of_file() {
    reset();
    let opened = Opened::open(Vec::new());
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    client
        .send(&format!(
            "{}\n{}",
            r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#,
            r#"{"id":"c_p","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#
        ))
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside quota");
    let watcher = opened.log.watch();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    await_log_end(watcher);
    hold.release();
    // The clock never advances: the answer is `closing`, not a cut.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns without the grace");
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_p");
    assert_eq!(acks.len(), 1, "{lines:?}");
    assert_eq!(acks[0]["payload"]["code"], "closing", "{lines:?}");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn close_ends_the_writers_while_another_handle_keeps_the_log() {
    reset();
    let opened = Opened::open(Vec::new());
    let client = subscribed_client(&opened);
    // A resumed process keeps its own `Arc<Log>` until it exits.
    let keep = Arc::clone(&opened.log);
    keep.append(&exited(), None, None).unwrap();
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns well before the grace, with the clock never advanced");
    let lines = read_to_eof(&client);
    assert!(
        lines.iter().any(|line| line["kind"] == "fiber_exited"),
        "the client reads every line through fiber_exited: {lines:?}"
    );
    assert!(
        keep.append(&notice(), None, None).is_err(),
        "the log is ended for the process that kept it"
    );
}

/// `docs/invocation.md`, "Lifecycle": a client that disconnected has its commands dropped, never delivered.
/// `docs/events.md`, "`clients`": `count` is the `full` connections attached, so the hang-up's detach writes `count` 0.
#[test]
fn a_client_hanging_up_after_the_stop_ends_its_writer_and_the_close_still_completes() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let client = subscribed_client(&opened);
    // The detach below runs only after the reader's read sees end of
    // file, so the zero-clients line proves the server consumed the
    // hang-up: the peer's FIN arrived (`docs/events.md`, "`clients`":
    // `count` is the `full` connections attached, so 0 means none
    // remain). A line appended before that can
    // reach the writer while the FIN is still in flight; its write
    // succeeds into the closing socket and the writer parks at the stop
    // with nothing left to fail it. Appending after the zero-clients
    // line makes this append the write that fails, which ends the writer.
    let mut hung_watcher = opened.log.watch();
    gate.conns.mark_stopped();
    drop(client);
    let (hung_tx, hung) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = hung_watcher.recv() {
            if line.kind == "clients"
                && line.payload.get("count").and_then(Value::as_u64) == Some(0)
            {
                if let Ok(()) = hung_tx.send(()) {}
                return;
            }
        }
    });
    Deadline::after(DEADLINE)
        .recv(&hung)
        .expect("the reader saw the hang-up and detached");
    opened.log.append(&notice(), None, None).unwrap();
    assert!(
        gate.conns
            .wait_while(DEADLINE, |state| state.writers_open() > 0),
        "the hanging-up client ends its writer"
    );
    let (_closer, done) = close_on_thread(opened.session, opened.log);
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the log's end lets the writer finish");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: the session stops waiting for a command still running at the grace and writes one `timeout` line to its diagnostic log for each connection it stopped waiting for.
#[test]
fn finish_leaves_the_entry_while_the_gate_is_stopped() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let reader = thread::spawn(|| {});
    let id = gate
        .conns
        .push_reader(reader, Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    gate.conns.mark_stopped();
    gate.conns.finish(id);
    assert!(
        gate.conns.published(id),
        "the entry stays until the connections are joined"
    );
    let detached = gate
        .conns
        .join_clients(opened.clock.as_ref(), opened.clock.now() + super::GRACE);
    assert_eq!(detached, 0, "a finished reader is joined, not detached");
    assert!(!gate.conns.published(id));
    close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: the session stops waiting for a command still running at the grace and writes one `timeout` line to its diagnostic log for each connection it stopped waiting for.
#[test]
fn join_clients_waits_for_a_reader_that_finishes_inside_the_grace() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // The reader stays inside its command until released, so the join must
    // wait: returning at once would reap it still unfinished and detach it.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (id_tx, id_rx) = mpsc::channel::<u64>();
    let finishing = Arc::clone(&gate);
    let reader = thread::spawn(move || {
        release_rx.recv().unwrap();
        finishing.conns.finish(id_rx.recv().unwrap());
    });
    let id = gate
        .conns
        .push_reader(reader, Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    id_tx.send(id).unwrap();
    gate.conns.mark_stopped();
    let until = opened.clock.now() + super::GRACE;
    let (parked_tx, parked_rx) = mpsc::channel::<bool>();
    let parked_clock = Arc::clone(&opened.clock);
    thread::spawn(move || {
        // The release always goes out, so a join that never parks fails
        // this test instead of hanging it.
        parked_tx
            .send(parked_clock.await_parked(until, Deadline::after(DEADLINE)))
            .unwrap();
        release_tx.send(()).unwrap();
    });
    let detached = gate.conns.join_clients(opened.clock.as_ref(), until);
    assert!(
        parked_rx.recv().unwrap(),
        "the join parks in its wait before the release"
    );
    assert_eq!(
        detached, 0,
        "a reader that finishes inside the grace is joined, not detached"
    );
    assert!(!gate.conns.published(id));
    close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: the quiesce waits for a cut writer's flush, so `fiber_exited` still reaches the connection after the cut.
#[test]
fn wait_cut_waits_for_the_cut_flush() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let clock = Arc::clone(&opened.clock);
    let reader = thread::spawn(|| {});
    let id = gate
        .conns
        .push_reader(reader, Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    gate.conns.note_owed(id, thread::current().id());
    gate.conns.cut_owed(&gate.session_id, |_| true);
    gate.conns.push_writer(id, thread::spawn(|| {}));
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = Arc::clone(&gate);
    let waiter_clock = Arc::clone(&clock);
    thread::spawn(move || {
        waiter.conns.wait_cut(waiter_clock.as_ref(), until);
        if let Ok(()) = done_tx.send(()) {}
    });
    let parked = clock.await_parked(until, Deadline::after(DEADLINE));
    // Release the waiter either way, so a failure here leaves no parked
    // thread behind.
    gate.conns.note_cut_flushed(id);
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the flush ends the wait for the cut");
    gate.conns.finish(id);
    assert!(parked, "wait_cut parks while a cut writer has not flushed");
    close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: a wait whose deadline already passed returns at once instead of waiting out the margin again.
#[test]
fn wait_cut_returns_once_the_deadline_passes() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let clock = Arc::clone(&opened.clock);
    let reader = thread::spawn(|| {});
    let id = gate
        .conns
        .push_reader(reader, Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    gate.conns.note_owed(id, thread::current().id());
    gate.conns.cut_owed(&gate.session_id, |_| true);
    gate.conns.push_writer(id, thread::spawn(|| {}));
    // The deadline is already past: the wait returns without parking, so a
    // quiesce that reaches it late never waits out the margin again.
    let until = clock.now();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = Arc::clone(&gate);
    let waiter_clock = Arc::clone(&clock);
    thread::spawn(move || {
        waiter.conns.wait_cut(waiter_clock.as_ref(), until);
        if let Ok(()) = done_tx.send(()) {}
    });
    let done = Deadline::after(DEADLINE).recv(&done_rx);
    // Release even a waiter that never returns on its own, so a failure
    // here leaves no parked thread behind.
    gate.conns.note_cut_flushed(id);
    gate.conns.finish(id);
    assert!(
        done.is_ok(),
        "wait_cut returns at once when the deadline already passed"
    );
    close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: the quiesce waits for the cut's flush through the whole flush margin past the grace, not just to the grace.
#[test]
fn settle_answers_waits_for_the_flush_through_the_whole_margin() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let clock = Arc::clone(&opened.clock);
    let reader = thread::current().id();
    gate.accepted.track(reader);
    gate.accepted.reserve(&CommandId("c_1".into()));
    let reading = thread::spawn(|| {});
    let id = gate
        .conns
        .push_reader(reading, Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    gate.conns.note_owed(id, reader);
    gate.conns.push_writer(id, thread::spawn(|| {}));
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    thread::spawn(move || {
        session.settle_answers();
        if let Ok(()) = done_tx.send(session) {}
    });
    assert!(
        clock.await_parked(until, Deadline::after(DEADLINE)),
        "settle_answers waits for the owed answer"
    );
    clock.advance_when_parked(Some(until), None, super::GRACE, &Deadline::after(DEADLINE));
    // The grace passed with the answer still owed: the session cut it and
    // now waits the flush margin for the cut writer's flush.
    let parked = clock.await_parked(until + super::CUT_FLUSH, Deadline::after(DEADLINE));
    // Release the waiter either way, so a failure here leaves no parked
    // thread behind.
    gate.conns.note_cut_flushed(id);
    let session = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the flush ends settle_answers");
    gate.conns.finish(id);
    assert!(
        parked,
        "settle_answers waits for the cut flush through the whole margin"
    );
    close_within(session, opened.log);
}

#[test]
fn quiet_readers_stops_a_reader_and_leaves_its_socket_open() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (mut peer, stream) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let shut = stream.try_clone().unwrap();
    let (read, stop) = ::support::stoppable::reader(stream).unwrap();
    let (left_tx, left) = mpsc::channel();
    let (id_tx, id_rx) = mpsc::channel();
    let leaving = Arc::clone(&gate);
    let reader = thread::spawn(move || {
        let mut read = io::BufReader::new(read);
        let mut line = String::new();
        io::BufRead::read_line(&mut read, &mut line).unwrap_or(0);
        if let Ok(()) = left_tx.send(()) {}
        if let Ok(id) = id_rx.recv() {
            leaving.conns.finish(id);
        }
    });
    let id = gate
        .conns
        .push_reader(
            reader,
            crate::client::ender(shutdown, stop),
            super::conns::shutter(shut),
        )
        .expect("the gate is running");
    if let Ok(()) = id_tx.send(id) {}
    gate.conns.mark_stopped();
    gate.conns.quiet_readers();
    Deadline::after(DEADLINE)
        .recv(&left)
        .expect("the quiet ends the reader's read");
    // The socket is still open: a line written now reaches the peer.
    writer.write_all(b"still open\n").unwrap();
    let mut got = [0u8; 11];
    peer.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"still open\n");
    close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the grace bounds the whole close: the session stops waiting for a command still running at the grace and writes one `timeout` line to its diagnostic log for each connection it stopped waiting for.
#[test]
fn a_reader_held_past_the_close_grace_is_detached_and_close_returns() {
    reset();
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (hold, entered) = Dispatch::new();
    wire_quota(&opened.session, &hold);
    let client = subscribed_client(&opened);
    // A resumed process keeps its `Arc<Log>`, and has written
    // `fiber_exited` before it closes.
    let keep = Arc::clone(&opened.log);
    keep_dir(&opened.log);
    keep.append(&exited(), None, None).unwrap();
    // The held `quota`, a `steer` and a second `quota` arrive in one write:
    // the last two sit in the reader's buffer.
    client
        .send(&format!(
            "{}\n{}\n{}",
            r#"{"id":"c_q","command":"quota","args":{"fetch":true}}"#,
            r#"{"id":"c_s","command":"steer","args":{"content":[{"type":"text","text":"more"}]}}"#,
            r#"{"id":"c_q2","command":"quota","args":{"fetch":true}}"#
        ))
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader is inside the first quota");
    let start = clock.now();
    let (dir, home, id) = (opened.dir.clone(), opened.home.clone(), opened.id.clone());
    let (closer, done) = close_on_thread(opened.session, opened.log);
    clock.advance_when_parked(
        Some(start + super::GRACE),
        Some(closer),
        super::GRACE,
        &Deadline::after(DEADLINE),
    );
    // The writer, cut, may need a moment to flush: the flush margin is the
    // only further wait.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returns with the reader still held");
    assert!(
        clock.now() <= start + super::GRACE + super::CUT_FLUSH,
        "close ends within the grace and the flush margin"
    );
    let lines = read_to_eof(&client);
    let acks = acks_for(&lines, "c_q");
    assert_eq!(acks.len(), 1, "one answer before end of file: {lines:?}");
    assert_eq!(acks[0]["payload"]["code"], "indeterminate", "{lines:?}");
    let diag = fs::read_to_string(home.join("logs").join(format!("session-{}.log", id.0)))
        .expect("the close wrote its diagnostic line");
    let written: Vec<Value> = diag
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(written.len(), 1, "{diag}");
    assert_eq!(written[0]["level"], "warn", "{diag}");
    assert_eq!(written[0]["process"], "session", "{diag}");
    assert_eq!(written[0]["session_id"], id.0.as_str(), "{diag}");
    assert_eq!(written[0]["code"], "timeout", "{diag}");
    let count = keep.count();
    // The detached reader dispatches its buffered `steer`, then enters the
    // quota again for `c_q2`: that entry shows the `steer` was dispatched.
    hold.release();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the detached reader reached its second quota");
    assert_eq!(
        log::last_kind(&dir).as_deref(),
        Some("fiber_exited"),
        "nothing lands after fiber_exited"
    );
    assert_eq!(keep.count(), count);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn accepted_records_reads_only_on_a_tracked_thread() {
    let accepted = super::Accepted::new();
    let me = thread::current().id();
    assert!(accepted.reserve(&CommandId("c_1".into())));
    assert_eq!(
        accepted.unanswered(me),
        0,
        "an untracked thread records nothing"
    );
    accepted.track(me);
    assert!(accepted.reserve(&CommandId("c_2".into())));
    assert!(
        !accepted.reserve(&CommandId("c_2".into())),
        "a repeat is not fresh"
    );
    assert_eq!(accepted.unanswered(me), 2, "a duplicate is recorded too");
    accepted.untrack(me);
    assert_eq!(accepted.unanswered(me), 0);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
#[test]
fn accepted_answers_one_occurrence_and_cuts_the_rest_in_read_order() {
    use super::Answered;
    let accepted = super::Accepted::new();
    let me = thread::current().id();
    accepted.track(me);
    for id in ["c_a", "c_b", "c_c", "c_b"] {
        accepted.reserve(&CommandId(id.into()));
    }
    assert_eq!(accepted.answered(me, "c_b", None), Answered::Write);
    accepted.resolve(me, "c_b", None);
    assert_eq!(accepted.unanswered(me), 3, "one occurrence of c_b left");
    assert_eq!(
        accepted.answered(me, "c_zzz", None),
        Answered::Write,
        "an id nobody read is written and changes nothing"
    );
    assert_eq!(accepted.unanswered(me), 3);
    assert_eq!(accepted.cut(me), ["c_a", "c_c", "c_b"]);
    assert_eq!(accepted.unanswered(me), 0);
    assert_eq!(
        accepted.answered(me, "c_a", None),
        Answered::Skip,
        "a cut id's own answer is skipped"
    );
    assert_eq!(accepted.answered(me, "c_a", None), Answered::Write, "once");
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn a_malformed_rejection_leaves_an_in_flight_id_unanswered() {
    use super::Answered;
    let accepted = super::Accepted::new();
    let me = thread::current().id();
    accepted.track(me);
    accepted.reserve(&CommandId("c_x".into()));
    assert_eq!(
        accepted.answered(me, "c_x", Some("malformed")),
        Answered::Write
    );
    assert_eq!(accepted.unanswered(me), 1, "c_x is still in flight");
    assert_eq!(
        accepted.answered(me, "c_x", Some("duplicate_command")),
        Answered::Write
    );
    accepted.resolve(me, "c_x", Some("duplicate_command"));
    assert_eq!(accepted.unanswered(me), 0, "any other rejection answers it");
}

/// A `write_loop` over a live log with a flight, as a subscribed
/// connection's writer runs: the test plays the reader on its own thread,
/// so the ids it reserves are the ones the writer owes an answer.
struct Flying {
    opened: Opened,
    gate: Arc<super::Gate>,
    injector: log::Injector,
    out: support::Shared,
    ended: mpsc::Receiver<()>,
    conn: u64,
}

impl Flying {
    /// Starts the writer thread. `queued` runs before it starts, so lines
    /// pushed there are in its queue when it begins.
    fn start(queued: impl FnOnce(&Opened, &log::Injector, &Arc<super::Gate>)) -> Self {
        let opened = Opened::open(Vec::new());
        let gate = Arc::clone(&opened.session.gate);
        let watcher = opened.log.watch();
        let injector = watcher.injector();
        let conn = gate
            .conns
            .push_reader(thread::spawn(|| {}), Box::new(|| {}), Box::new(|| {}))
            .expect("the gate is running");
        let reader = thread::current().id();
        gate.accepted.track(reader);
        gate.conns.note_owed(conn, reader);
        queued(&opened, &injector, &gate);
        let out = support::Shared::new();
        let (_switches, switch_rx) = mpsc::channel();
        let (done_tx, ended) = mpsc::channel();
        let flight = crate::client::Flight {
            gate: Arc::clone(&gate),
            conn,
            reader,
        };
        let sink = out.clone();
        thread::spawn(move || {
            crate::client::write_loop(watcher, Box::new(sink), false, switch_rx, Some(&flight));
            if let Ok(()) = done_tx.send(()) {}
        });
        Self {
            opened,
            gate,
            injector,
            out,
            ended,
            conn,
        }
    }

    /// The writer has not ended: nothing could have ended it, so no wait.
    fn not_ended(&self) -> bool {
        self.ended.try_recv().is_err()
    }

    fn wait_ended(&self) {
        Deadline::after(DEADLINE)
            .recv(&self.ended)
            .expect("the writer ended");
    }

    fn answer(&self, id: &str) -> contract::Envelope {
        crate::session::envelope(
            &self.gate.session_id,
            self.opened.clock.as_ref(),
            &Event::CommandAccepted(CommandAccepted {
                command_id: CommandId(id.into()),
                result: None,
            }),
        )
    }

    fn stop(&self) {
        self.injector.push_kept(crate::client::control_line(
            &self.gate.session_id,
            crate::client::STOP,
            0,
        ));
    }

    fn finish(self) {
        self.gate.conns.finish(self.conn);
        close_within(self.opened.session, self.opened.log);
    }
}

/// Cuts every live connection whose reader still owes an answer: the
/// test's cut, mirroring what the session cuts when its grace passes.
fn cut_owed(gate: &super::Gate) {
    gate.conns.cut_owed(&gate.session_id, |reader| {
        gate.accepted.unanswered(reader) > 0
    });
}

fn read_ids(out: &support::Shared) -> Vec<(String, String)> {
    out.lines()
        .iter()
        .map(|line| {
            (
                line["kind"].as_str().unwrap_or_default().to_owned(),
                line["payload"]["command_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn past_the_end_stop_does_not_end_a_writer_that_owes_an_answer() {
    reset();
    let flying = Flying::start(|_, _, gate| {
        gate.accepted.reserve(&CommandId("c_1".into()));
        gate.conns.mark_stopped();
    });
    flying.opened.log.end_watchers();
    flying.stop();
    flying.injector.push_kept(flying.answer("c_1"));
    flying.wait_ended();
    assert_eq!(
        read_ids(&flying.out),
        [("command_accepted".to_owned(), "c_1".to_owned())]
    );
    flying.finish();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
#[test]
fn a_cut_writes_indeterminate_for_what_is_unanswered_and_ends() {
    reset();
    let flying = Flying::start(|_, _, gate| {
        gate.accepted.reserve(&CommandId("c_1".into()));
        gate.accepted.reserve(&CommandId("c_2".into()));
    });
    flying.opened.log.end_watchers();
    cut_owed(&flying.gate);
    flying.wait_ended();
    let lines = flying.out.lines();
    assert_eq!(read_ids(&flying.out).len(), 2, "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|line| line["kind"] == "command_rejected"
                && line["payload"]["code"] == "indeterminate"),
        "{lines:?}"
    );
    assert_eq!(lines[0]["payload"]["command_id"], "c_1", "read order");
    assert_eq!(lines[1]["payload"]["command_id"], "c_2");
    // The command's own answer after the cut reaches nobody.
    flying.injector.push_kept(flying.answer("c_1"));
    assert_eq!(flying.out.lines().len(), 2);
    flying.finish();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn an_own_answer_queued_ahead_of_cut_is_written_and_not_cut() {
    reset();
    let flying = Flying::start(|opened, injector, gate| {
        gate.accepted.reserve(&CommandId("c_1".into()));
        gate.accepted.reserve(&CommandId("c_2".into()));
        let answer = crate::session::envelope(
            &gate.session_id,
            opened.clock.as_ref(),
            &Event::CommandAccepted(CommandAccepted {
                command_id: CommandId("c_1".into()),
                result: None,
            }),
        );
        injector.push_kept(answer);
        cut_owed(gate);
        injector.push_kept(crate::client::control_line(&gate.session_id, super::CUT, 0));
    });
    flying.opened.log.end_watchers();
    flying.wait_ended();
    assert_eq!(
        read_ids(&flying.out),
        [
            ("command_accepted".to_owned(), "c_1".to_owned()),
            ("command_rejected".to_owned(), "c_2".to_owned())
        ],
        "c_1 keeps its own answer; only c_2 is cut"
    );
    let lines = flying.out.lines();
    assert_eq!(lines[1]["payload"]["code"], "indeterminate", "{lines:?}");
    flying.finish();
}

#[test]
fn a_stop_before_the_end_ends_the_writer_unless_the_session_is_stopping() {
    reset();
    let flying = Flying::start(|_, _, _| {});
    flying.stop();
    flying.wait_ended();
    flying.finish();

    let flying = Flying::start(|_, _, gate| gate.conns.mark_stopped());
    let gate = Arc::clone(&flying.gate);
    let (parked_tx, parked) = mpsc::channel();
    *lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::WriterParkedAtStop
            && let Ok(()) = parked_tx.send(())
        {}
    }));
    flying.stop();
    Deadline::after(DEADLINE)
        .recv(&parked)
        .expect("the writer parks at STOP while the session is stopping");
    assert!(
        flying.not_ended(),
        "once the session is stopping, STOP is recorded and the writer goes on to the end"
    );
    flying.opened.log.end_watchers();
    flying.wait_ended();
    flying.finish();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it: a cut that reaches the writer while its reader owes nothing flushes nothing, so the later quiesce wait still waits for the real flush.
#[test]
fn a_cut_with_nothing_owed_leaves_the_flush_unmarked() {
    reset();
    let flying = Flying::start(|_, injector, gate| {
        injector.push_kept(crate::client::control_line(&gate.session_id, super::CUT, 0));
    });
    flying.stop();
    flying.wait_ended();
    assert!(
        flying.out.lines().is_empty(),
        "a cut with nothing owed writes nothing"
    );
    // The skipped cut flushed nothing: marking the connection cut now still
    // leaves the flush outstanding, so the quiesce wait parks.
    flying
        .gate
        .conns
        .cut_owed(&flying.gate.session_id, |_| true);
    flying
        .gate
        .conns
        .push_writer(flying.conn, thread::spawn(|| {}));
    let clock = Arc::clone(&flying.opened.clock);
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = Arc::clone(&flying.gate);
    let waiter_clock = Arc::clone(&clock);
    thread::spawn(move || {
        waiter.conns.wait_cut(waiter_clock.as_ref(), until);
        if let Ok(()) = done_tx.send(()) {}
    });
    let parked = clock.await_parked(until, Deadline::after(DEADLINE));
    // Release the waiter either way, so a failure here leaves no parked
    // thread behind.
    flying.gate.conns.note_cut_flushed(flying.conn);
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the flush ends the quiesce wait");
    assert!(
        parked,
        "the quiesce wait still waits: a cut with nothing owed flushed nothing"
    );
    flying.finish();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it: past the log's end, a cut line is dropped, never written to the client.
#[test]
fn past_end_cut_line_is_dropped() {
    reset();
    let flying = Flying::start(|_, _, gate| {
        gate.accepted.reserve(&CommandId("c_1".into()));
        gate.conns.mark_stopped();
    });
    // The STOP is queued before the log ends, so the writer consumes it
    // before the end; the log ends while it waits, so the CUT below
    // arrives past the end.
    let gate = Arc::clone(&flying.gate);
    let (past_tx, past_end) = mpsc::channel();
    *lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::WriterPastEnd
            && let Ok(()) = past_tx.send(())
        {}
    }));
    flying.stop();
    flying.opened.log.end_watchers();
    Deadline::after(DEADLINE)
        .recv(&past_end)
        .expect("the writer reads past the log's end");
    flying.injector.push_kept(crate::client::control_line(
        &flying.gate.session_id,
        super::CUT,
        0,
    ));
    flying.injector.push_kept(flying.answer("c_1"));
    flying.wait_ended();
    assert_eq!(
        read_ids(&flying.out),
        [("command_accepted".to_owned(), "c_1".to_owned())]
    );
    flying.finish();
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it: past the log's end, a level line with no switch pending changes nothing and is never written to the client.
#[test]
fn past_end_level_line_without_a_switch_changes_nothing() {
    reset();
    let flying = Flying::start(|_, _, gate| {
        gate.accepted.reserve(&CommandId("c_1".into()));
        gate.conns.mark_stopped();
    });
    // The STOP is queued before the log ends, so the writer consumes it
    // before the end; the log ends while it waits, so the LEVEL below
    // arrives past the end.
    let gate = Arc::clone(&flying.gate);
    let (past_tx, past_end) = mpsc::channel();
    *lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::WriterPastEnd
            && let Ok(()) = past_tx.send(())
        {}
    }));
    flying.stop();
    flying.opened.log.end_watchers();
    Deadline::after(DEADLINE)
        .recv(&past_end)
        .expect("the writer reads past the log's end");
    // `client::level::LEVEL` is private to the client module; this is its value.
    flying.injector.push_kept(crate::client::control_line(
        &flying.gate.session_id,
        "doors.level",
        0,
    ));
    flying.injector.push_kept(flying.answer("c_1"));
    flying.wait_ended();
    assert_eq!(
        read_ids(&flying.out),
        [("command_accepted".to_owned(), "c_1".to_owned())]
    );
    flying.finish();
}

#[test]
fn process_calls_on_an_ordinary_connection_are_rejected() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_1", "full")).unwrap();
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            for (id, line) in [
                (
                    "c_2",
                    r#"{"id":"c_2","command":"call","args":{"call":"fiber.tool","args":[]}}"#,
                ),
                ("c_3", r#"{"id":"c_3","command":"ready"}"#),
            ] {
                client.send(line).unwrap();
                let rejected = answer_of(&client, id);
                assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
                assert_eq!(
                    rejected["payload"]["code"], "invalid_arguments",
                    "{rejected}"
                );
                assert!(
                    inbox.try_recv().is_err(),
                    "a refused process command delivers nothing"
                );
            }
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}
