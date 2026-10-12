//! `doors::steer` against a real session socket: the wire order, the
//! commit point, the probe, and every outcome (`docs/delegates.md`,
//! "Talking to a delegate").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use support::*;

use contract::SessionId;
use contract::inbox::Delivery;
use contract::inbox::Rejection;
use contract::shapes::ContentPart;
use contract::{ErrorCode, SCHEMA_VERSION};
use doors::{Steered, mint};
use fakes::Deadline;

/// A commit that counts its calls and answers as told.
struct Commit {
    calls: Arc<AtomicUsize>,
    allow: bool,
    called: mpsc::Sender<()>,
}

impl Commit {
    fn run(&mut self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _sent = self.called.send(());
        self.allow
    }
}

/// Runs `steer` on a thread; its outcome arrives on the first returned
/// channel under [`DEADLINE`], one item per `on_queued` run on the
/// second. The commit counts its calls and answers `allow`; `on_queued`
/// counts its own and signals. A test that needs the probe's proof
/// before it answers waits for the signal first.
fn spawn_steer(
    home: std::path::PathBuf,
    id: SessionId,
    command: String,
    text: String,
    calls: Arc<AtomicUsize>,
    queued: Arc<AtomicUsize>,
    allow: bool,
) -> (
    mpsc::Receiver<Steered>,
    mpsc::Receiver<()>,
    thread::JoinHandle<()>,
) {
    let (done_tx, done_rx) = mpsc::channel();
    let (queued_tx, queued_rx) = mpsc::channel();
    let (called_tx, _) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut commit = Commit {
            calls,
            allow,
            called: called_tx,
        };
        let mut on_queued = move || {
            queued.fetch_add(1, Ordering::SeqCst);
            let _sent = queued_tx.send(());
        };
        let outcome = doors::steer(
            &home,
            &id,
            &command,
            &text,
            &mut || commit.run(),
            &mut on_queued,
        );
        let _sent = done_tx.send(outcome);
    });
    (done_rx, queued_rx, handle)
}

fn steered(rx: &mpsc::Receiver<Steered>) -> Steered {
    Deadline::after(DEADLINE).recv(rx).expect("steer answered")
}

/// One command line the socket read, parsed.
fn commanded(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut raw = String::new();
    reader.read_line(&mut raw).unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// The `command_accepted` line for `command`, speaking this build's schema.
fn accepted(command: &str) -> Vec<u8> {
    let line = serde_json::json!({
        "kind": "command_accepted",
        "schema_version": SCHEMA_VERSION,
        "payload": {"command_id": command},
    });
    format!("{line}\n").into_bytes()
}

/// Binds a socket served by the test at `home/run/<id>`.
fn listen(home: &std::path::Path, id: &SessionId) -> UnixListener {
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    UnixListener::bind(run.join(&id.0)).unwrap()
}

#[test]
fn an_accepted_steer_delivers_one_text_part_after_the_probe_proved_it_queued() {
    let opened = Opened::open(vec![]);
    let home = opened.home.clone();
    let id = opened.id.clone();
    let command = mint("c_");
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, queued_rx, steered_in) = spawn_steer(
        home,
        id,
        command,
        "use the other test file".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let Delivery::Steer(message, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the steer is delivered")
            else {
                panic!("a steer is delivered");
            };
            assert_eq!(
                message.content,
                [ContentPart::Text {
                    text: "use the other test file".to_owned(),
                }],
                "the delivered content is one text part with the message"
            );
            // The probe's answer proves the message is queued: the
            // accept below is answered only after it arrived, so its
            // order against the steer's own answer is not a race.
            Deadline::after(DEADLINE)
                .recv(&queued_rx)
                .expect("the probe proved it queued before the accept");
            ack.0(Ok(None));
            Ok(())
        })
        .unwrap();
    assert_eq!(steered(&answered), Steered::Accepted);
    steered_in.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    assert_eq!(
        queued.load(Ordering::SeqCst),
        1,
        "on_queued ran once, before the accept"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn a_rejected_steer_reports_its_code_and_message() {
    let opened = Opened::open(vec![]);
    let home = opened.home.clone();
    let id = opened.id.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, _queued, steered_in) = spawn_steer(
        home,
        id,
        mint("c_"),
        "use the other test file".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let Delivery::Steer(_, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the steer is delivered")
            else {
                panic!("a steer is delivered");
            };
            ack.0(Err(Rejection {
                code: ErrorCode::MessageRefused,
                message: "not now".to_owned(),
            }));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        steered(&answered),
        Steered::Rejected(doors::failure(ErrorCode::MessageRefused, "not now"))
    );
    steered_in.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    close_within(opened.session, opened.log);
}

#[test]
fn a_steer_without_a_socket_is_gone_with_no_commit() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, _queued, steered_in) = spawn_steer(
        home,
        SessionId(mint("s_")),
        mint("c_"),
        "hi".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    assert_eq!(steered(&answered), Steered::Gone);
    steered_in.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "commit never ran");
    assert_eq!(queued.load(Ordering::SeqCst), 0, "nothing queued");
}

#[test]
fn a_steer_sends_its_command_id_and_a_reused_one_is_rejected_duplicate() {
    let opened = Opened::open(vec![]);
    let home = opened.home.clone();
    let id = opened.id.clone();
    let command = mint("c_");
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (first, _queued, steered_in) = spawn_steer(
        home.clone(),
        id.clone(),
        command.clone(),
        "use the other test file".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let Delivery::Steer(_, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the steer is delivered")
            else {
                panic!("a steer is delivered");
            };
            ack.0(Ok(None));
            // The first steer line reached the session, so its id is
            // reserved: the same id sent again is a duplicate, never a
            // second message.
            let mut commit = || true;
            let mut on_queued = || {};
            let outcome = doors::steer(&home, &id, &command, "again", &mut commit, &mut on_queued);
            assert!(
                matches!(
                    &outcome,
                    Steered::Rejected(failure) if failure.code == ErrorCode::DuplicateCommand
                ),
                "a reused command id is rejected duplicate: {outcome:?}"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(steered(&first), Steered::Accepted);
    steered_in.join().expect("the steer thread ended");
    close_within(opened.session, opened.log);
}

#[test]
fn the_steer_wire_is_subscribe_then_steer_then_a_probe() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let listener = listen(&home, &id);
    let command = mint("c_");
    let wired = command.clone();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let sub = commanded(&mut reader);
        assert_eq!(sub["command"], "subscribe");
        assert_eq!(sub["args"]["level"], "summary");
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        let steered = commanded(&mut reader);
        assert_eq!(
            steered["id"], wired,
            "the steer line carries exactly the command id passed in"
        );
        assert_eq!(steered["command"], "steer");
        assert_eq!(
            steered["args"]["content"],
            serde_json::json!([{"type": "text", "text": "wire order"}])
        );
        let probe = commanded(&mut reader);
        assert_eq!(probe["command"], "tools");
        // The probe's answer first, so `on_queued` runs, then the steer's.
        let probe_id = probe["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&probe_id)).unwrap();
        writer.write_all(&accepted(&wired)).unwrap();
        writer.flush().unwrap();
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, _queued, steered_in) = spawn_steer(
        home,
        id,
        command,
        "wire order".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    assert_eq!(steered(&answered), Steered::Accepted);
    steered_in.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    assert_eq!(
        queued.load(Ordering::SeqCst),
        1,
        "the probe proved it queued"
    );
    served.join().expect("the server held the wire order");
}

#[test]
fn a_line_before_the_probe_answer_does_not_queue_the_steer() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let listener = listen(&home, &id);
    let command = mint("c_");
    let wired = command.clone();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let sub = commanded(&mut reader);
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        let steered = commanded(&mut reader);
        assert_eq!(steered["command"], "steer");
        assert_eq!(steered["id"], wired);
        let probe = commanded(&mut reader);
        assert_eq!(probe["command"], "tools");
        // An answer to neither the probe nor the steer, then the
        // connection closes: the probe's own answer is never written.
        let unrelated = mint("c_");
        writer.write_all(&accepted(&unrelated)).unwrap();
        writer.flush().unwrap();
        drop(writer);
        drop(reader);
        drop(stream);
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, _queued_rx, steered_in) = spawn_steer(
        home,
        id,
        command,
        "before the probe".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        true,
    );
    assert_eq!(steered(&answered), Steered::Gone);
    steered_in.join().expect("the steer thread ended");
    served.join().expect("the server closed the connection");
    assert_eq!(
        queued.load(Ordering::SeqCst),
        0,
        "a line before the probe's answer proves nothing"
    );
}

#[test]
fn commit_runs_only_after_the_subscribe_is_answered() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let listener = listen(&home, &id);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let sub = commanded(&mut reader);
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        // The acknowledgement is held: nothing may commit before it.
        Deadline::after(DEADLINE)
            .recv(&release_rx)
            .expect("the test releases the acknowledgement");
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        // The steer line proves the commit returned true; it is never
        // answered, so this attempt ends gone.
        let steered = commanded(&mut reader);
        assert_eq!(steered["command"], "steer");
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (called_tx, called_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let home_client = home.clone();
    let id_client = id.clone();
    let counted = Arc::clone(&calls);
    let held_back = thread::spawn(move || {
        let mut commit = || {
            counted.fetch_add(1, Ordering::SeqCst);
            let _sent = called_tx.send(());
            true
        };
        let mut on_queued = || {
            queued.fetch_add(1, Ordering::SeqCst);
        };
        let outcome = doors::steer(
            &home_client,
            &id_client,
            &mint("c_"),
            "held",
            &mut commit,
            &mut on_queued,
        );
        let _sent = done_tx.send(outcome);
    });
    // Held acknowledgement: the commit must not arrive.
    let short = fakes::Deadline::after(Duration::from_millis(500));
    assert!(
        short.recv(&called_rx).is_err(),
        "commit is not called before the subscribe is answered"
    );
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&called_rx)
        .expect("commit runs once the subscribe is answered");
    assert_eq!(steered(&done_rx), Steered::Gone);
    held_back.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    served.join().expect("the server read the steer line");
}

#[test]
fn a_refused_commit_writes_no_steer_line() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let listener = listen(&home, &id);
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut raw = String::new();
        reader.read_line(&mut raw).unwrap();
        let sub: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        // Past the acknowledgement the client must send nothing: read to
        // end of file and collect what arrived.
        let mut lines = Vec::new();
        loop {
            raw.clear();
            let bytes = reader.read_line(&mut raw).unwrap();
            if bytes == 0 {
                break;
            }
            lines.push(raw.trim_end().to_owned());
        }
        lines
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (answered, _queued, steered_in) = spawn_steer(
        home,
        id,
        mint("c_"),
        "withheld".to_owned(),
        Arc::clone(&calls),
        Arc::clone(&queued),
        false,
    );
    assert_eq!(steered(&answered), Steered::Withheld);
    steered_in.join().expect("the steer thread ended");
    let lines = served.join().expect("the server read to end of file");
    assert!(
        lines.is_empty(),
        "no steer line was written past the subscribe: {lines:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    assert_eq!(queued.load(Ordering::SeqCst), 0, "nothing queued");
}

#[test]
fn a_blocked_send_holds_no_lock_and_ends_gone_on_close() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let listener = listen(&home, &id);
    let (accepted_tx, accepted_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let sub = commanded(&mut reader);
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        accepted_tx.send(()).unwrap();
        // Past any socket buffer the message cannot fit: stop reading
        // here, holding the connection open until the test closes it.
        Deadline::after(DEADLINE)
            .recv(&release_rx)
            .expect("the test closes the connection");
        drop(stream);
        drop(writer);
    });
    // 8 MiB, past any socket buffer, compared by length only.
    let big = "x".repeat(8 * 1024 * 1024);
    let gate = Arc::new(std::sync::Mutex::new(()));
    let calls = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let home_client = home.clone();
    let id_client = id.clone();
    let gated = Arc::clone(&gate);
    let counted = Arc::clone(&calls);
    let queued_in = Arc::clone(&queued);
    let blocked = thread::spawn(move || {
        let mut commit = || {
            // Locked and released before the write: the test locks this
            // same mutex while the write blocks.
            let _held = gated.lock().unwrap();
            let _sent = entered_tx.send(());
            counted.fetch_add(1, Ordering::SeqCst);
            true
        };
        let mut on_queued = || {
            queued_in.fetch_add(1, Ordering::SeqCst);
        };
        let outcome = doors::steer(
            &home_client,
            &id_client,
            &mint("c_"),
            &big,
            &mut commit,
            &mut on_queued,
        );
        let _sent = done_tx.send(outcome);
    });
    Deadline::after(DEADLINE)
        .recv(&accepted_rx)
        .expect("the subscribe was accepted");
    // The commit locked and released the mutex: locking it here proves no
    // lock is held across the write, while the 8 MiB message blocks.
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the commit entered");
    let held = gate.lock().unwrap();
    drop(held);
    release_tx.send(()).unwrap();
    assert_eq!(steered(&done_rx), Steered::Gone);
    blocked.join().expect("the steer thread ended");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    served.join().expect("the server closed");
}
