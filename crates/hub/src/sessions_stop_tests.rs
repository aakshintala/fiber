//! Tests for the sessions-stop loop: every session shuts down through
//! its own shutdown, bounded, with the interleavings forced by hooks and
//! barriers instead of waited for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::Value;

use super::*;
use crate::Starter;
use crate::fake::FakeStarter;
use crate::testkit;
use fakes::Deadline;
use fakes::clock::FakeClock;

/// One deadline per join: the loop never parks past it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<FakeClock>,
}

impl Temp {
    fn new(prefix: &str) -> Self {
        let (held, dir) = testkit::with_run(prefix);
        Self {
            dir,
            held,
            clock: FakeClock::new(),
        }
    }

    fn hub(&self, starter: FakeStarter) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = Arc::clone(&self.clock) as Arc<dyn Clock>;
        Arc::new(testkit::hub(&self.dir, Arc::new(starter), timed))
    }

    fn run(&self, id: &SessionId) -> PathBuf {
        self.dir.join("run").join(&id.0)
    }
}

/// Starts one fake session through `starter` under `home`.
fn start(starter: &FakeStarter, n: u64) -> SessionId {
    let id = SessionId(testkit::id(n));
    starter
        .start(&id, Path::new("/w"), None, &[], false, None)
        .unwrap();
    id
}

/// Runs the loop on a thread: the receiver carries its outcome.
fn stop_async(hub: &Arc<Hub>) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let hub = Arc::clone(hub);
    thread::Builder::new()
        .name("sessions-stop-test".to_owned())
        .spawn(move || {
            tx.send(run(&hub)).unwrap_or(());
        })
        .unwrap();
    rx
}

/// Joins a loop thread under one deadline: a loop that waits out the
/// bound instead of answering fails here.
fn join(rx: mpsc::Receiver<Outcome>) -> Outcome {
    Deadline::after(DEADLINE).recv_or_fail(&rx, "the sessions stop")
}

fn command_of(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

#[test]
fn stops_a_running_session_through_subscribe_then_close() {
    let temp = Temp::new("ss-stop");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let probe = starter.clone();
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![id.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    // The socket is gone, as a session's exit leaves it.
    assert!(UnixStream::connect(temp.run(&id)).is_err());
    // This connection subscribed before it closed, each line on a fresh
    // hub-minted id: a close-first mutant fails these.
    let conns = probe.received_by_connection();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].len(), 2);
    let subscribe = command_of(&conns[0][0]);
    let close = command_of(&conns[0][1]);
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["args"]["level"], "summary");
    assert_eq!(close["command"], "close");
    assert_eq!(close["args"]["now"], true);
    let subscribe_id = subscribe["id"].as_str().unwrap();
    let close_id = close["id"].as_str().unwrap();
    assert!(subscribe_id.starts_with("c_hub_stop_"));
    assert!(close_id.starts_with("c_hub_stop_"));
    assert_ne!(subscribe_id, close_id);
}

#[test]
fn stops_every_running_session_leaving_no_socket_behind() {
    let temp = Temp::new("ss-stop-all");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let first = start(&starter, 1);
    let second = start(&starter, 2);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![first.clone(), second.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(UnixStream::connect(temp.run(&first)).is_err());
    assert!(UnixStream::connect(temp.run(&second)).is_err());
}

#[test]
fn close_before_subscribe_is_rejected_not_subscribed() {
    let temp = Temp::new("ss-strict");
    let starter = FakeStarter::strict(&temp.dir);
    let id = start(&starter, 1);
    let mut stream = UnixStream::connect(temp.run(&id)).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let line = serde_json::json!({"id": "c_1", "command": "close", "args": {"now": true}});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).unwrap();
    let mut read = BufReader::new(stream.try_clone().unwrap());
    let answer = support::framing::read_line(&mut read, support::framing::MAX_LINE)
        .unwrap()
        .unwrap();
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], "c_1");
    assert_eq!(answer["payload"]["code"], "not_subscribed");
}

#[test]
fn a_refused_socket_is_skipped() {
    let temp = Temp::new("ss-refused");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let live = start(&starter, 1);
    // A listener dropped without unlinking: the path refuses, as a dead
    // session's socket does.
    let stale = SessionId(testkit::id(2));
    let stale_listener = UnixListener::bind(temp.run(&stale)).unwrap();
    drop(stale_listener);
    let hub = temp.hub(starter);
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![live]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(temp.run(&stale).exists());
}

#[test]
fn a_session_that_exits_between_enumeration_and_connect_is_skipped() {
    let temp = Temp::new("ss-exit");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let live = start(&starter, 1);
    let gone = SessionId(testkit::id(2));
    // Held open so the listing finds a live path; the hook exits it
    // after the enumeration, before any connect.
    let _held = UnixListener::bind(temp.run(&gone)).unwrap();
    let hub = temp.hub(starter);
    let path = temp.run(&gone);
    *lock(&hub.on_scan_listed) = Some(Box::new(move || {
        fs::remove_file(&path).unwrap();
    }));
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![live]);
    assert!(outcome.unconfirmed.is_empty());
}

#[test]
fn a_session_started_mid_scan_is_shut_down_by_the_rescan() {
    let temp = Temp::new("ss-midscan");
    let starter = Arc::new(FakeStarter::exit_on_close(&temp.dir));
    let first = start(&starter, 1);
    let hub = temp.hub((*starter).clone());
    let second = SessionId(testkit::id(2));
    let for_hook = Arc::clone(&starter);
    let second_for_hook = second.clone();
    *lock(&hub.on_scan_listed) = Some(Box::new(move || {
        // The starter binds fully before the scan resumes: the rescan
        // finds a running session, not a half-bound one.
        for_hook
            .start(&second_for_hook, Path::new("/w"), None, &[], false, None)
            .unwrap();
    }));
    let outcome = join(stop_async(&hub));
    assert_eq!(outcome.confirmed, vec![first.clone(), second.clone()]);
    assert!(outcome.unconfirmed.is_empty());
    assert!(UnixStream::connect(temp.run(&second)).is_err());
}

#[test]
fn the_loop_waits_for_binds_in_flight_before_scanning() {
    let temp = Temp::new("ss-inflight");
    let starter = FakeStarter::exit_on_close(&temp.dir);
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    // A held bind keeps the first zero-wait parked: the parked signal is
    // the hook, fired on the loop's thread.
    hub.inflight_binds.fetch_add(1, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::channel();
    *lock(&hub.on_inflight_wait) = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
    }));
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&entered_rx, "the zero-wait park");
    // Parked, not done: no zero-wait means the outcome arrives before the
    // hook ever fires.
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    hub.inflight_binds.fetch_sub(1, Ordering::SeqCst);
    hub.tick.wake();
    let outcome = join(rx);
    assert_eq!(outcome.confirmed, vec![id]);
    assert!(outcome.unconfirmed.is_empty());
}

#[test]
fn abort_mid_fan_out_names_the_session_still_running() {
    let temp = Temp::new("ss-abort");
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let id = start(&starter, 1);
    let hub = temp.hub(starter);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    *lock(&hub.on_fanout_wait) = Some(Box::new(move || {
        waiting_tx.send(()).unwrap_or(());
    }));
    let started = temp.clock.now();
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&waiting_rx, "the fan-out wait park");
    hub.request_stop();
    hub.tick.wake();
    let outcome = join(rx);
    assert!(outcome.confirmed.is_empty());
    assert_eq!(outcome.unconfirmed, vec![id.clone()]);
    // Still running: its socket accepts, and the clock never moved.
    assert!(UnixStream::connect(temp.run(&id)).is_ok());
    assert_eq!(temp.clock.now(), started);
}

#[test]
fn abort_in_the_first_zero_wait_probes_without_waiting() {
    let temp = Temp::new("ss-abort-wait");
    let live = SessionId(testkit::id(1));
    let _held = UnixListener::bind(temp.run(&live)).unwrap();
    let stale = SessionId(testkit::id(2));
    let stale_listener = UnixListener::bind(temp.run(&stale)).unwrap();
    drop(stale_listener);
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let hub = temp.hub(starter);
    // The held bind keeps the serving thread in its first zero-wait; the
    // hook is the parked signal.
    hub.inflight_binds.fetch_add(1, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::channel();
    *lock(&hub.on_inflight_wait) = Some(Box::new(move || {
        entered_tx.send(()).unwrap_or(());
    }));
    let started = temp.clock.now();
    let rx = stop_async(&hub);
    Deadline::after(DEADLINE).recv_or_fail(&entered_rx, "the zero-wait park");
    hub.request_stop();
    hub.tick.wake();
    let outcome = join(rx);
    assert!(outcome.confirmed.is_empty());
    // The live listener is named; the stale path is skipped, never named.
    assert_eq!(outcome.unconfirmed, vec![live]);
    assert_eq!(temp.clock.now(), started);
}

#[test]
fn a_split_acknowledgement_parses_whole_after_its_tail_arrives() {
    let temp = Temp::new("ss-split-ack");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, mut session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_split"}}))
    .unwrap();
    full.push(b'\n');
    let cut = full.len() / 2;
    // The prefix lands before the reader exists: the parked wait below
    // can only be the post-prefix wait, never an empty-socket one, so
    // the tail cannot arrive before the reader consumes the prefix.
    session.write_all(&full[..cut]).unwrap();
    let stream = client.try_clone().unwrap();
    let read = BufReader::new(client);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut read = read;
        let ack = read_ack(&mut read, &stream, &hub, "c_split", end);
        done_tx.send(matches!(ack, Ack::Accepted)).unwrap_or(());
    });
    // The head holds no line: the parked wait below is the signal the
    // reader kept it and waits for the tail.
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the acknowledgement read parks for its tail"
    );
    session.write_all(&full[cut..]).unwrap();
    assert!(
        Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("the read ends"),
        "the split acknowledgement is accepted whole"
    );
}

use std::io::Read as _;
use std::net::{TcpListener, TcpStream};
use std::os::fd::IntoRawFd as _;

/// A refused loopback port queues first, then reads its queued error:
/// refused means gone, so the loop skips the session and continues.
#[test]
#[allow(unsafe_code, reason = "the probe socket is opened and dialled by hand")]
fn a_refused_connect_reads_gone() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    // SAFETY: a fresh socket fd this test owns; the assert below
    // checks the call before the fd is used.
    let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    assert!(raw >= 0, "the probe socket opens");
    // SAFETY: `raw` is the open socket above, owned by no one yet.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: flag reads and writes touch only the owned probe socket.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL, 0) };
    assert_ne!(flags, -1, "the probe socket offers its flags");
    assert_ne!(
        // SAFETY: flag reads and writes touch only the owned probe socket.
        unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1,
        "the probe socket goes non-blocking"
    );
    let addr = libc::sockaddr_in {
        sin_family: libc::sa_family_t::try_from(libc::AF_INET).unwrap(),
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: 0x7f00_0001u32.to_be(),
        },
        sin_zero: [0; 8],
    };
    let connected = {
        // SAFETY: `addr` is a local of at least `len` bytes; `connect`
        // reads only those bytes and takes no ownership.
        unsafe {
            libc::connect(
                raw,
                &addr as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>().try_into().unwrap(),
            )
        }
    };
    assert_eq!(connected, -1, "the closed port refuses");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EINPROGRESS),
        "a refused loopback port queues first"
    );
    assert!(
        raw_poll(raw, libc::POLLOUT, 5_000) & libc::POLLOUT != 0,
        "the refusal surfaces"
    );
    assert!(
        matches!(check_connected(socket.as_fd()), Err(ConnectError::Gone)),
        "a refused connect is gone"
    );
}

/// A connected socket reads no error: the loop proceeds to the session.
#[test]
fn a_connected_connect_reads_ok() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    assert!(
        check_connected(socket.as_fd()).is_ok(),
        "a connected socket reads no error"
    );
}

/// A reset loopback connection reads neither refused nor connected: the
/// loop neither skips the session nor proceeds, but waits out the bound.
#[test]
#[allow(unsafe_code, reason = "linger-zero is set by hand to reset")]
fn a_reset_connect_reads_unconfirmed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let reset = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        let ok = {
            // SAFETY: `reset` is a local of exactly the option's
            // length; `setsockopt` reads only it and takes no ownership.
            unsafe {
                libc::setsockopt(
                    conn.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &reset as *const libc::linger as *const libc::c_void,
                    std::mem::size_of::<libc::linger>().try_into().unwrap(),
                )
            }
        };
        assert_eq!(ok, 0, "linger-zero arms the reset");
        // Dropping a linger-zero socket resets the connection.
    });
    let client = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    server.join().unwrap();
    // The reset arrives as a poll error: the test waits for the event,
    // never sleeps for it.
    let mut revents = 0;
    for _ in 0..50 {
        revents = raw_poll(client.as_raw_fd(), libc::POLLERR | libc::POLLHUP, 100);
        if revents & libc::POLLERR != 0 {
            break;
        }
    }
    assert!(
        revents & libc::POLLERR != 0,
        "the reset surfaces as a poll error"
    );
    assert!(
        matches!(
            check_connected(client.as_fd()),
            Err(ConnectError::Unconfirmed)
        ),
        "a reset connect is unconfirmed"
    );
}

/// A quiet socket polls not ready: replacing the poll with true would
/// wait where there is nothing to wait for.
#[test]
fn a_quiet_socket_polls_not_ready() {
    let (socket, _peer) = UnixStream::pair().unwrap();
    assert!(
        !poll_ready(socket.as_fd(), libc::POLLIN),
        "a quiet socket polls not ready"
    );
}

/// Readiness reports each single-flag combination: readable alone is
/// ready, writable alone is ready, and neither is not.
#[test]
fn readiness_reports_each_single_flag_combination() {
    let (readable, peer) = UnixStream::pair().unwrap();
    (&peer).write_all(b"x").unwrap();
    assert!(
        poll_ready(readable.as_fd(), libc::POLLIN),
        "a readable socket polls ready"
    );
    let (writable, _held) = UnixStream::pair().unwrap();
    assert!(
        poll_ready(writable.as_fd(), libc::POLLOUT),
        "a fresh pair polls writable"
    );
    let (quiet, _kept) = UnixStream::pair().unwrap();
    assert!(
        !poll_ready(quiet.as_fd(), libc::POLLIN),
        "a quiet socket polls not ready"
    );
}

/// A full send buffer makes the bounded write wait, not fail: once the
/// peer drains, the line lands whole.
#[test]
fn a_full_send_buffer_retries_until_drained() {
    let temp = Temp::new("ss-write-full");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (writer, peer) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    // Fill the send buffer: small writes until the kernel says to wait.
    let filler = vec![0u8; 65_536];
    for _ in 0..128 {
        match (&writer).write(&filler) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("filling the send buffer: {error}"),
        }
    }
    let line = serde_json::json!({"id": "c_write_full", "command": "subscribe",
        "args": {"level": "summary"}});
    let mut expected = serde_json::to_vec(&line).unwrap();
    expected.push(b'\n');
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    let queued = writer.try_clone().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(write_line_bounded(&queued, &line, &hub, end))
            .unwrap_or(());
    });
    assert!(
        temp.clock.await_parked(end, Deadline::after(DEADLINE)),
        "the bounded write waits for room"
    );
    // Drain the filler: the freed room unblocks the write.
    let mut chunk = vec![0u8; 65_536];
    loop {
        match (&peer).read(&mut chunk) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("draining the peer: {error}"),
        }
    }
    assert!(
        matches!(
            Deadline::after(DEADLINE)
                .recv(&done_rx)
                .expect("the write ends"),
            Wrote::Done
        ),
        "the bounded write lands once drained"
    );
    // The line lands last, after the filler.
    let mut received = Vec::new();
    loop {
        match (&peer).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => received.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("reading the line: {error}"),
        }
    }
    assert!(
        received.ends_with(&expected),
        "the line lands whole after the drain"
    );
}

/// A partial line at end of input still answers: without its newline the
/// acknowledgement parses once the close ends the input.
#[test]
fn a_partial_line_at_end_of_input_still_answers() {
    for tail in ["", "\r"] {
        let temp = Temp::new("ss-partial-ack");
        let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
        let (client, mut session) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let stream = client.try_clone().unwrap();
        let mut read = BufReader::new(client);
        let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_partial"}}))
        .unwrap();
        full.extend_from_slice(tail.as_bytes());
        // No newline: the close ends the input with the line partial.
        session.write_all(&full).unwrap();
        drop(session);
        let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
        assert!(
            matches!(
                read_ack(&mut read, &stream, &hub, "c_partial", end),
                Ack::Accepted
            ),
            "a partial acknowledgement with tail {tail:?} is accepted at end of input"
        );
    }
}

/// A newline-terminated acceptance counts even when the socket closes:
/// the close ends the stream, it does not erase the answer.
#[test]
fn an_accepted_answer_counts_even_when_the_socket_closes() {
    let temp = Temp::new("ss-accepted-close");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let mut full = serde_json::to_vec(&serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_closed"}}))
    .unwrap();
    full.push(b'\n');
    (&session).write_all(&full).unwrap();
    drop(session);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_closed", end),
            Ack::Accepted
        ),
        "a newline-terminated acceptance counts before the close"
    );
}

/// A long line far below the length edge still reaches its
/// acknowledgement: the bound is tens of megabytes out, not a product
/// of the bytes seen so far.
#[test]
fn a_long_line_below_the_edge_still_reaches_its_ack() {
    let temp = Temp::new("ss-length-below");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let (client, session) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = client.try_clone().unwrap();
    let mut read = BufReader::new(client);
    let ack = serde_json::json!({"kind": "command_accepted", "ts": 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": "c_long"}});
    let session_thread = thread::spawn(move || {
        // A megabyte of line: far below the edge for the real bound,
        // far past it for a product of pending and chunk.
        let junk = vec![b'x'; 1 << 20];
        (&session).write_all(&junk).unwrap();
        (&session).write_all(b"\n").unwrap();
        let mut line = serde_json::to_vec(&ack).unwrap();
        line.push(b'\n');
        (&session).write_all(&line).unwrap();
    });
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        matches!(
            read_ack(&mut read, &stream, &hub, "c_long", end),
            Ack::Accepted
        ),
        "a megabyte line is skipped, not fatal"
    );
    session_thread.join().unwrap();
}

/// An unconnected socket fails its first read: the acknowledgement is
/// gone, never waited on.
#[test]
#[allow(unsafe_code, reason = "the probe socket stays unconnected by hand")]
fn an_unconnected_socket_is_gone_not_waited_on() {
    let temp = Temp::new("ss-unconnected");
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    // SAFETY: a fresh socket fd this test owns; the assert below
    // checks the call before the fd is used.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(raw >= 0, "the probe socket opens");
    // SAFETY: `raw` is the open socket above, owned by no one yet.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let unix = UnixStream::from(socket);
    let stream = unix.try_clone().unwrap();
    let mut read = BufReader::new(unix);
    let end = temp.clock.origin() + SESSIONS_STOP_WITHIN;
    assert!(
        matches!(read_ack(&mut read, &stream, &hub, "c_dead", end), Ack::Gone),
        "an unconnected socket is gone"
    );
}

/// A closed fd fails to go non-blocking: the error propagates instead of
/// a silent blocking socket.
#[test]
#[allow(
    unsafe_code,
    reason = "the probe fd is closed by hand to fail the call"
)]
fn a_closed_fd_fails_nonblocking() {
    // SAFETY: a fresh socket fd this test owns; the assert below
    // checks the call before the fd is used.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(raw >= 0, "the probe socket opens");
    // SAFETY: `raw` is the open socket above, owned by no one yet.
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let number = owned.into_raw_fd();
    // SAFETY: `number` is the fd just released; closing it touches
    // nothing else, and the borrow below makes no call that could
    // hand it out before the check runs.
    unsafe { libc::close(number) };
    // SAFETY: the fd stays closed through the check below: nothing
    // runs between the close and the call it fails.
    let closed = unsafe { BorrowedFd::borrow_raw(number) };
    assert_ne!(
        set_nonblocking(closed),
        0,
        "a closed fd cannot go non-blocking"
    );
}

/// An address beside the family still computes its bound: family bytes
/// plus path bytes plus the NUL.
#[test]
fn an_address_beside_the_family_computes_its_bound() {
    let name = format!("s_{}", "a".repeat(100));
    let path = Path::new("run").join(&name);
    let (_, len) = unix_addr(&path).unwrap();
    assert_eq!(
        len as usize,
        std::mem::size_of::<libc::sa_family_t>() + path.as_os_str().as_bytes().len() + 1,
        "the length covers the family, the bytes and the NUL"
    );
    let too_long = Path::new("run").join(format!("s_{}", "a".repeat(104)));
    assert!(
        unix_addr(&too_long).is_err(),
        "a path filling sun_path is refused"
    );
}
