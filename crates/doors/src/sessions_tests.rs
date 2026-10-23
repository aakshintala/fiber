//! Socket discovery, summary protocol and the one injected-clock deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions and fixtures"
)]

use super::*;
use contract::events::{SessionState, SessionStatus};
use contract::shapes::{Tokens, Usage};
use fakes::CancelToken;
use fakes::clock::FakeClock;
use fakes::{Deadline, TempDir};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

const ID: &str = "s_0000000000000001";
const OTHER: &str = "s_0000000000000002";

fn status(name: &str) -> SessionStatus {
    SessionStatus {
        name: name.into(),
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
        spend: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: Default::default(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        delegates: 0,
        jobs: 0,
        project: "p".into(),
        clients: 0,
        closing: false,
    }
}

fn ack(command: &str) -> Value {
    serde_json::json!({"kind":"command_accepted", "schema_version":contract::SCHEMA_VERSION, "payload":{"command_id":command}})
}

fn summary(s: &SessionStatus) -> Value {
    serde_json::json!({"kind":"session_status", "payload":s})
}

struct Home {
    temp: TempDir,
    clock: Arc<FakeClock>,
}

impl Home {
    fn new() -> Self {
        Self {
            temp: TempDir::new("sl"),
            clock: FakeClock::new(),
        }
    }
    fn run(&self) -> PathBuf {
        config::run_dir(self.temp.path())
    }
    fn client(&self) -> SessionSockets {
        SessionSockets::new(self.temp.path(), self.clock.clone())
    }
    fn listener(&self, id: &str) -> UnixListener {
        fs::create_dir_all(self.run()).unwrap();
        UnixListener::bind(self.run().join(id)).unwrap()
    }
    fn start(&self, cancel: Arc<CancelToken>) -> Receiver<Result<Vec<Running>, ListError>> {
        let client = self.client();
        let (tx, rx) = mpsc::channel();
        thread::spawn(
            move || {
                if let Ok(()) = tx.send(client.list(cancel.as_ref())) {}
            },
        );
        rx
    }
    fn advance(&self, deadline: &Deadline) {
        self.clock.advance_when_parked(
            Some(self.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap()),
            None,
            LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
            deadline,
        );
    }
}

// Socket I/O is bounded by one wall deadline, including the peer's own
// waits. The release sender belongs to the test and drops on assertion failure.
struct Peer {
    subscribed: Receiver<String>,
    ended: Receiver<()>,
    release: Option<Sender<()>>,
}

impl Peer {
    fn new(
        home: &Home,
        id: &str,
        script: impl FnOnce(&str, &mut UnixStream) + Send + 'static,
    ) -> Self {
        let listener = home.listener(id);
        let (sub_tx, subscribed) = mpsc::channel();
        let (end_tx, ended) = mpsc::channel();
        thread::spawn(move || {
            let deadline = Deadline::after(fakes::deadline::WAITS);
            listener.set_nonblocking(true).unwrap();
            let mut fds = [PollFd::new(&listener, PollFlags::IN)];
            let bound = rustix::event::Timespec::try_from(deadline.left()).unwrap();
            rustix::event::poll(&mut fds, Some(&bound)).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            // macOS inherits the listener's nonblocking mode; peer I/O must
            // block up to its socket timeout, not fail before the client closes.
            stream.set_nonblocking(false).unwrap();
            // Socket timeouts reject zero, even when the deadline has expired.
            let timeout = deadline.left().max(Duration::from_millis(1));
            stream.set_read_timeout(Some(timeout)).unwrap();
            stream.set_write_timeout(Some(timeout)).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let line: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(line["command"], "subscribe");
            assert_eq!(line["args"], serde_json::json!({"level":"summary"}));
            let command = line["id"].as_str().unwrap();
            assert!(command.starts_with("c_"));
            if let Ok(()) = sub_tx.send(command.to_owned()) {}
            script(command, &mut stream);
            let timeout = deadline.left().max(Duration::from_millis(1));
            // macOS rejects setsockopt with EINVAL once the client has closed;
            // the read below then returns EOF at once, so the bound is moot.
            match stream.set_read_timeout(Some(timeout)) {
                Err(e) if e.kind() == io::ErrorKind::InvalidInput => {}
                other => other.unwrap(),
            }
            let mut rest = Vec::new();
            match stream.read_to_end(&mut rest) {
                Ok(_) => assert!(rest.is_empty()),
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
                Err(e) => panic!("peer EOF: {e}"),
            }
            end_tx.send(()).unwrap();
        });
        Self {
            subscribed,
            ended,
            release: None,
        }
    }
    fn answer(home: &Home, id: &str, s: SessionStatus) -> Self {
        Self::new(home, id, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            writeln!(stream, "{}", summary(&s)).unwrap();
        })
    }
    fn hold(home: &Home, id: &str, partial: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let (ready, written) = mpsc::channel();
        let mut peer = Self::new(home, id, move |id, stream| {
            if partial {
                writeln!(stream, "{}", ack(id)).unwrap();
                stream.write_all(b"{\"kind\":\"session_status\"").unwrap();
            }
            ready.send(id.to_owned()).unwrap();
            // The test owns tx, including on unwind.
            if let Ok(()) = rx.recv() {}
        });
        peer.subscribed = written;
        peer.release = Some(tx);
        peer
    }
    fn release(&mut self) {
        self.release.take();
    }
    fn subscribed(&self, deadline: &Deadline) -> String {
        deadline.recv_or_fail(&self.subscribed, "peer read summary subscription")
    }
    fn ended(&self, deadline: &Deadline) {
        deadline.recv_or_fail(&self.ended, "peer saw EOF");
    }
}

#[test]
fn missing_run_is_empty_and_unreadable_run_is_an_error() {
    let home = Home::new();
    assert_eq!(home.client().list(&CancelToken::new()), Ok(vec![]));
    fs::create_dir(home.run()).unwrap();
    fs::set_permissions(home.run(), fs::Permissions::from_mode(0o000)).unwrap();
    let result = home.client().list(&CancelToken::new());
    fs::set_permissions(home.run(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(ListError::Io { path, .. }) if path == home.run()));
}

#[test]
fn only_valid_session_names_are_connected_and_stale_paths_are_omitted() {
    let home = Home::new();
    let invalid = [
        "hub",
        "s_000000000000000",
        "s_00000000000000000",
        "s_AAAAAAAAAAAAAAAA",
        "notes",
        "s_gggggggggggggggg",
    ];
    let listeners: Vec<_> = invalid.iter().map(|id| home.listener(id)).collect();
    let stale = home.listener(OTHER);
    drop(stale);
    fs::write(home.run().join("s_0000000000000003"), "not a socket").unwrap();
    let s = status("answer");
    let peer = Peer::answer(&home, ID, s.clone());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = deadline
        .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "listing")
        .unwrap();
    assert_eq!(
        result,
        vec![Running {
            id: SessionId(ID.into()),
            status: Some(s)
        }]
    );
    peer.ended(&deadline);
    for listener in listeners {
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn every_answer_is_returned_in_id_order_without_advancing_time() {
    let home = Home::new();
    let ids = ["s_0000000000000003", OTHER, ID];
    let peers: Vec<_> = ids
        .iter()
        .map(|id| Peer::answer(&home, id, status(id)))
        .collect();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = deadline
        .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "all answers")
        .unwrap();
    let expected: Vec<_> = ids
        .iter()
        .rev()
        .map(|id| Running {
            id: SessionId((*id).into()),
            status: Some(status(id)),
        })
        .collect();
    assert_eq!(result, expected);
    for peer in peers {
        peer.ended(&deadline);
    }
}

#[test]
fn a_silent_peer_does_not_delay_an_answers_close_and_is_listed_at_cutoff() {
    let home = Home::new();
    let mut hold = Peer::hold(&home, OTHER, false);
    let s = status("ready");
    let answer = Peer::answer(&home, ID, s.clone());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(Arc::new(CancelToken::new()));
    hold.subscribed(&deadline);
    answer.ended(&deadline);
    home.advance(&deadline);
    assert_eq!(
        deadline.recv_or_fail(&result, "cutoff").unwrap(),
        vec![
            Running {
                id: SessionId(ID.into()),
                status: Some(s)
            },
            Running {
                id: SessionId(OTHER.into()),
                status: None
            }
        ]
    );
    hold.release();
    hold.ended(&deadline);
}

#[test]
fn an_unfinished_status_is_not_an_answer() {
    let home = Home::new();
    let mut peer = Peer::hold(&home, ID, true);
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(Arc::new(CancelToken::new()));
    peer.subscribed(&deadline);
    home.advance(&deadline);
    assert_eq!(
        deadline.recv_or_fail(&result, "partial cutoff").unwrap(),
        vec![Running {
            id: SessionId(ID.into()),
            status: None
        }]
    );
    peer.release();
    peer.ended(&deadline);
}

#[test]
fn the_first_json_line_must_accept_our_command_at_our_version() {
    for case in 0..6 {
        let home = Home::new();
        let peer = Peer::new(&home, ID, move |id, stream| {
            let mut first = ack(id);
            match case {
                0 => first["kind"] = "command_rejected".into(),
                1 => first["payload"]["command_id"] = "c_other".into(),
                2 => first["schema_version"] = (contract::SCHEMA_VERSION + 1).into(),
                3 => {}
                4 => {}
                5 => first["kind"] = "extensions_loaded".into(),
                _ => unreachable!(),
            }
            writeln!(stream, "{first}").unwrap();
            if case == 4 {
                writeln!(stream, "{}", serde_json::json!({"kind":"session_status", "payload":{"name":"missing workspace"}})).unwrap();
            }
            if case == 3 {
                stream.shutdown(std::net::Shutdown::Write).unwrap();
            }
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let result = deadline
            .recv_or_fail(
                &home.start(Arc::new(CancelToken::new())),
                "invalid protocol",
            )
            .unwrap();
        assert_eq!(
            result,
            vec![Running {
                id: SessionId(ID.into()),
                status: None
            }],
            "case {case}"
        );
        peer.ended(&deadline);
    }
}

#[test]
fn unrelated_and_non_json_lines_are_skipped_after_the_ack() {
    let home = Home::new();
    let peer = Peer::new(&home, ID, |id, stream| {
        writeln!(stream, "not JSON").unwrap();
        writeln!(stream, "{}", ack(id)).unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"kind":"extensions_loaded", "payload":{}})
        )
        .unwrap();
        writeln!(stream, "not JSON either").unwrap();
        writeln!(stream, "{}", summary(&status("ready"))).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    assert_eq!(
        deadline
            .recv_or_fail(
                &home.start(Arc::new(CancelToken::new())),
                "status after other lines"
            )
            .unwrap(),
        vec![Running {
            id: SessionId(ID.into()),
            status: Some(status("ready"))
        }]
    );
    peer.ended(&deadline);
}

#[test]
fn cancellation_before_discovery_connects_to_nothing() {
    let home = Home::new();
    let listener = home.listener(ID);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(home.client().list(&cancel), Err(ListError::Cancelled));
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_wakes_the_poll_without_advancing_time() {
    let home = Home::new();
    let mut peer = Peer::hold(&home, ID, false);
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let result = home.start(cancel.clone());
    peer.subscribed(&deadline);
    assert!(home.clock.await_parked(
        home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
        fakes::Deadline::after(fakes::deadline::WAITS)
    ));
    cancel.cancel();
    assert_eq!(
        deadline.recv_or_fail(&result, "cancelled poll"),
        Err(ListError::Cancelled)
    );
    peer.release();
    peer.ended(&deadline);
}

#[test]
fn each_subscription_uses_a_fresh_command_id() {
    let home = Home::new();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let first = Peer::answer(&home, ID, status("first"));
    let result = home.start(Arc::new(CancelToken::new()));
    let a = first.subscribed(&deadline);
    assert!(
        deadline.recv_or_fail(&result, "first listing").unwrap()[0]
            .status
            .is_some()
    );
    first.ended(&deadline);
    fs::remove_file(home.run().join(ID)).unwrap();
    let second = Peer::answer(&home, ID, status("second"));
    let result = home.start(Arc::new(CancelToken::new()));
    let b = second.subscribed(&deadline);
    assert!(
        deadline.recv_or_fail(&result, "second listing").unwrap()[0]
            .status
            .is_some()
    );
    assert_ne!(a, b);
    assert!(a.starts_with("c_"));
    assert!(b.starts_with("c_"));
    second.ended(&deadline);
}

#[test]
fn the_line_cap_allows_the_boundary_and_rejects_one_more_unterminated_byte() {
    for size in [MAX_LINE - 1, MAX_LINE, MAX_LINE + 1] {
        let home = Home::new();
        let base = serde_json::to_vec(&summary(&status(""))).unwrap().len();
        let mut s = status(&"x".repeat(size - base));
        s.parent = None;
        let expected = s.clone();
        let peer = Peer::new(&home, ID, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            let bytes = serde_json::to_vec(&summary(&s)).unwrap();
            assert_eq!(bytes.len(), size);
            stream.write_all(&bytes).unwrap();
            if size <= MAX_LINE {
                stream.write_all(b"\n").unwrap();
            }
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let result = deadline
            .recv_or_fail(&home.start(Arc::new(CancelToken::new())), "line cap")
            .unwrap();
        assert_eq!(result.len(), 1);
        match result[0].status.as_ref() {
            Some(s) => {
                assert!(size <= MAX_LINE);
                assert_eq!(s.name.len(), expected.name.len());
                assert!(s == &expected);
            }
            None => assert_eq!(size, MAX_LINE + 1),
        }
        peer.ended(&deadline);
    }
}

// Signals only on the poll after the read seam has seen the whole prefix.
struct PrefixClock {
    clock: Arc<FakeClock>,
    prefix_read: Arc<std::sync::atomic::AtomicBool>,
    polled: Sender<()>,
}

impl Clock for PrefixClock {
    fn now(&self) -> Instant {
        self.clock.now()
    }
    fn wall(&self) -> std::time::SystemTime {
        self.clock.wall()
    }
    fn sleep(&self, duration: Duration) {
        self.clock.sleep(duration);
    }
    fn subscribe(&self, wake: std::sync::Weak<dyn Wake>) {
        self.clock.subscribe(wake);
    }
    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.clock.wait_until(until, &mut |bound| {
            if self
                .prefix_read
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.polled.send(()).unwrap();
            }
            wait(bound);
        });
    }
}

#[test]
fn a_literal_sized_split_summary_survives_below_one_mib() {
    let home = Home::new();
    let expected = status(&"x".repeat(8192));
    let sent_status = expected.clone();
    let (release, held) = mpsc::channel();
    let peer = Peer::new(&home, ID, move |id, stream| {
        writeln!(stream, "{}", ack(id)).unwrap();
        let bytes = serde_json::to_vec(&summary(&sent_status)).unwrap();
        stream.write_all(&bytes[..4096]).unwrap();
        if held.recv().is_ok() {
            stream.write_all(&bytes[4096..]).unwrap();
            stream.write_all(b"\n").unwrap();
        }
    });
    let prefix_read = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (polled, next_poll) = mpsc::channel();
    let client = SessionSockets::new(
        home.temp.path(),
        Arc::new(PrefixClock {
            clock: home.clock.clone(),
            prefix_read: prefix_read.clone(),
            polled,
        }),
    );
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut seen = Vec::new();
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                let n = stream.read(chunk)?;
                seen.extend_from_slice(&chunk[..n]);
                if let Some(end) = seen.iter().position(|byte| *byte == b'\n')
                    && seen.len() - end > 4096
                {
                    prefix_read.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(n)
            });
        done.send(answer).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    deadline.recv_or_fail(&next_poll, "prefix processed and next poll entered");
    release.send(()).unwrap();
    let entries = deadline
        .recv_or_fail(&result, "complete split summary")
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, SessionId(ID.into()));
    let actual = entries[0].status.as_ref().expect("complete status");
    assert_eq!(actual.name.len(), 8192);
    assert!(actual == &expected, "complete status content");
    peer.ended(&deadline);
}

#[test]
fn would_block_after_readiness_preserves_the_subscription() {
    let home = Home::new();
    let expected = status("ready after WouldBlock");
    let peer = Peer::answer(&home, ID, expected.clone());
    let client = home.client();
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut injected = false;
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                if !injected {
                    injected = true;
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                stream.read(chunk)
            });
        done.send((injected, answer)).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let (injected, answer) = deadline.recv_or_fail(&result, "summary after WouldBlock");
    assert!(injected);
    assert_eq!(
        answer,
        Ok(vec![Running {
            id: SessionId(ID.into()),
            status: Some(expected),
        }])
    );
    peer.ended(&deadline);
}

#[test]
fn a_hard_read_error_closes_the_peer_without_advancing_time() {
    let home = Home::new();
    // The peer stays open, waiting for EOF, so neither EOF nor a clock
    // advance can rescue a read error incorrectly treated as WouldBlock.
    let peer = Peer::new(&home, ID, |id, stream| {
        let mut bytes = serde_json::to_vec(&ack(id)).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).unwrap();
    });
    let client = home.client();
    let cutoff = home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let mut injected = false;
        let answer =
            client.list_until_with_read(cutoff, &CancelToken::new(), &mut |stream, chunk| {
                if !injected {
                    injected = true;
                    return Err(io::ErrorKind::ConnectionReset.into());
                }
                stream.read(chunk)
            });
        done.send((injected, answer)).unwrap();
    });
    let deadline = Deadline::after(fakes::deadline::WAITS);
    peer.ended(&deadline);
    let (injected, answer) = deadline.recv_or_fail(&result, "hard read error closes listing");
    assert!(injected);
    assert_eq!(
        answer,
        Ok(vec![Running {
            id: SessionId(ID.into()),
            status: None
        }])
    );
}

#[test]
fn discovery_at_the_cutoff_connects_to_nothing_and_cancel_takes_priority() {
    let home = Home::new();
    let listeners: Vec<_> = [ID, OTHER, "s_0000000000000003"]
        .into_iter()
        .map(|id| home.listener(id))
        .collect();
    assert_eq!(
        home.client()
            .list_until(home.clock.now(), &CancelToken::new()),
        Ok(vec![])
    );
    for listener in listeners {
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        home.client()
            .list_until(home.clock.now() + LIST_DEADLINE, &cancel),
        Err(ListError::Cancelled)
    );
}

#[test]
fn unending_short_lines_cannot_outlive_the_cutoff_or_cancellation() {
    for (json, cancel_call) in [(false, false), (true, false), (false, true)] {
        let home = Home::new();
        let (sent, written) = mpsc::channel();
        let peer = Peer::new(&home, ID, move |id, stream| {
            writeln!(stream, "{}", ack(id)).unwrap();
            let line = if json {
                b"{\"kind\":\"notice\",\"payload\":{}}\n".as_slice()
            } else {
                b"x\n".as_slice()
            };
            for _ in 0..1_000 {
                stream.write_all(line).unwrap();
            }
            sent.send(()).unwrap();
            while stream.write_all(line).is_ok() {}
        });
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let cancel = Arc::new(CancelToken::new());
        let result = home.start(cancel.clone());
        deadline.recv_or_fail(&written, "first 1000 flood lines written");
        if cancel_call {
            assert!(home.clock.await_parked(
                home.clock.origin() + LIST_DEADLINE.checked_sub(CLEANUP_MARGIN).unwrap(),
                fakes::Deadline::after(fakes::deadline::WAITS)
            ));
            cancel.cancel();
            assert_eq!(
                deadline.recv_or_fail(&result, "flood cancelled"),
                Err(ListError::Cancelled)
            );
        } else {
            home.advance(&deadline);
            assert_eq!(
                deadline.recv_or_fail(&result, "flood cutoff").unwrap(),
                vec![Running {
                    id: SessionId(ID.into()),
                    status: None
                }]
            );
        }
        peer.ended(&deadline);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_backlog_is_not_an_accepted_connection() {
    let home = Home::new();
    let listener = home.listener(ID);
    let address = rustix::net::SocketAddrUnix::new(home.run().join(ID)).unwrap();
    let mut filled = Vec::new();
    let deadline = Deadline::after(fakes::deadline::WAITS);
    loop {
        assert!(!deadline.left().is_zero(), "backlog filled within deadline");
        let socket = crate::hub::nonblocking_socket().unwrap();
        match rustix::net::connect(&socket, &address) {
            Ok(()) => filled.push(socket),
            Err(rustix::io::Errno::AGAIN) => break,
            Err(e) => panic!("filling backlog: {e}"),
        }
    }
    assert_eq!(home.client().list(&CancelToken::new()), Ok(vec![]));
    drop(listener);
}

const FROM: &str = "s_00000000000000aa";

fn accepted(command: &str) -> Value {
    serde_json::json!({"kind":"command_accepted", "schema_version":contract::SCHEMA_VERSION, "payload":{"command_id":command}})
}

fn rejected(command: &str, code: &str, message: &str) -> Value {
    serde_json::json!({"kind":"command_rejected", "schema_version":contract::SCHEMA_VERSION, "payload":{"command_id":command, "code":code, "message":message}})
}

fn exchange_pair(deadline: &Deadline) -> (UnixStream, UnixStream) {
    let (client, peer) = UnixStream::pair().unwrap();
    client.set_nonblocking(true).unwrap();
    peer.set_read_timeout(Some(deadline.left().max(Duration::from_millis(1))))
        .unwrap();
    peer.set_write_timeout(Some(deadline.left().max(Duration::from_millis(1))))
        .unwrap();
    (client, peer)
}

fn spawn_message_exchange<W, R>(
    sockets: SessionSockets,
    path: PathBuf,
    stream: UnixStream,
    text: String,
    cancel: Arc<CancelToken>,
    io: ExchangeIo<W, R>,
) -> Receiver<Result<(), MessageError>>
where
    W: FnMut(&mut UnixStream, &[u8]) -> io::Result<usize> + Send + 'static,
    R: FnMut(&mut UnixStream, &mut [u8]) -> io::Result<usize> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let from = SessionId(FROM.into());
        let mut io = io;
        let message = contract::commands::Message {
            from_session_id: from,
            parent_session_id: None,
            text,
        };
        let result = sockets.message_exchange(&path, stream, &message, cancel.as_ref(), &mut io);
        tx.send(result).unwrap();
    });
    rx
}

fn send_ignored(stream: &mut UnixStream, line: &str) {
    // The client may have closed already; the assertion below is on the
    // client's answer, so a refused answer write is ignored.
    match stream.write_all(line.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
}

#[test]
fn message_write_error_is_unreachable_not_retried() {
    let home = Home::new();
    let path = home.run().join(ID);
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let (client, mut peer) = exchange_pair(&deadline);
    let result = spawn_message_exchange(
        home.client(),
        path,
        client,
        "hello".into(),
        Arc::new(CancelToken::new()),
        ExchangeIo {
            write: {
                let mut failed = false;
                move |stream: &mut UnixStream, buf: &[u8]| {
                    if !failed {
                        failed = true;
                        return Err(io::Error::other("injected write error"));
                    }
                    stream.write(buf)
                }
            },
            read: |stream: &mut UnixStream, chunk: &mut [u8]| stream.read(chunk),
        },
    );
    // A failed write reports unreachable at once: the peer reads end of
    // file, with no command line before it.
    let mut reader = BufReader::new(peer.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap() != 0 {
        // The guard-ignoring mutant retried the failed write and pipelined
        // the commands: answer them so it returns Ok(()) fast, not at the
        // deadline, and the assertion below still kills it.
        let first: Value = serde_json::from_str(&first).unwrap();
        let mut second = String::new();
        reader.read_line(&mut second).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(first["command"], "subscribe");
        assert_eq!(second["command"], "message");
        send_ignored(
            &mut peer,
            &format!("{}\n", accepted(first["id"].as_str().unwrap())),
        );
        send_ignored(
            &mut peer,
            &format!("{}\n", accepted(second["id"].as_str().unwrap())),
        );
    }
    assert_eq!(
        deadline.recv_or_fail(&result, "write error is unreachable"),
        Err(MessageError::Unreachable)
    );
}

#[test]
fn message_read_error_is_unreachable_not_retried() {
    let home = Home::new();
    let path = home.run().join(ID);
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let (client, mut peer) = exchange_pair(&deadline);
    let result = spawn_message_exchange(
        home.client(),
        path,
        client,
        "hello".into(),
        Arc::new(CancelToken::new()),
        ExchangeIo {
            write: |stream: &mut UnixStream, buf: &[u8]| stream.write(buf),
            read: {
                let mut failed = false;
                move |stream: &mut UnixStream, chunk: &mut [u8]| {
                    if !failed {
                        failed = true;
                        return Err(io::Error::other("injected read error"));
                    }
                    stream.read(chunk)
                }
            },
        },
    );
    let (exchange, _) = read_exchange(&peer);
    send_ignored(&mut peer, &format!("{}\n", accepted(&exchange.sub)));
    send_ignored(&mut peer, &format!("{}\n", accepted(&exchange.msg)));
    assert_eq!(
        deadline.recv_or_fail(&result, "read error is unreachable"),
        Err(MessageError::Unreachable)
    );
    // The sender returns on the injected error without reading the two
    // answers above, so its close lands with unread bytes queued: Linux
    // reports that close as ECONNRESET, macOS as EOF. Both mean closed.
    match peer.read(&mut [0u8; 1]) {
        Ok(0) => {}
        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
        Ok(_) => panic!("the sender writes no third line after its answer"),
        Err(e) => panic!("the sender closes straight after its answer: {e}"),
    }
}

#[test]
fn message_prefix_at_the_cap_completes() {
    // At one below and at exactly MAX_LINE, an unterminated prefix is not
    // over the cap: the noise line is skipped once completed and the
    // answer is delivered. The at-cap case kills both buffer-len mutants.
    for size in [MAX_LINE - 1, MAX_LINE] {
        let home = Home::new();
        let path = home.run().join(ID);
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let (client, mut peer) = exchange_pair(&deadline);
        let (sig_tx, sig_rx) = mpsc::channel();
        let fired = Arc::new(AtomicBool::new(false));
        let total = Arc::new(AtomicUsize::new(0));
        let result = spawn_message_exchange(
            home.client(),
            path,
            client,
            "hello".into(),
            Arc::new(CancelToken::new()),
            ExchangeIo {
                write: |stream: &mut UnixStream, buf: &[u8]| stream.write(buf),
                read: {
                    let fired = Arc::clone(&fired);
                    let total = Arc::clone(&total);
                    move |stream: &mut UnixStream, chunk: &mut [u8]| {
                        let n = stream.read(chunk)?;
                        if total.fetch_add(n, Ordering::SeqCst) + n >= size
                            && !fired.swap(true, Ordering::SeqCst)
                        {
                            match sig_tx.send(()) {
                                Ok(()) | Err(_) => {}
                            }
                        }
                        Ok(n)
                    }
                },
            },
        );
        let (exchange, _) = read_exchange(&peer);
        peer.write_all(&vec![b'x'; size]).unwrap();
        deadline.recv_or_fail(&sig_rx, "client read the capped prefix");
        // This newline ends the noise line at exactly `size`; the answer
        // after it is delivered.
        send_ignored(&mut peer, &format!("\n{}\n", accepted(&exchange.msg)));
        assert_eq!(
            deadline.recv_or_fail(&result, "capped prefix completes"),
            Ok(()),
            "size {size}"
        );
        assert_closed(&peer);
    }
}

#[test]
fn message_prefix_over_the_cap_is_unreachable() {
    // One byte past the cap with no newline ends the connection, while the
    // peer holds its side open: no later answer can rescue it.
    let home = Home::new();
    let path = home.run().join(ID);
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let (client, mut peer) = exchange_pair(&deadline);
    let result = spawn_message_exchange(
        home.client(),
        path,
        client,
        "hello".into(),
        Arc::new(CancelToken::new()),
        ExchangeIo {
            write: |stream: &mut UnixStream, buf: &[u8]| stream.write(buf),
            read: |stream: &mut UnixStream, chunk: &mut [u8]| stream.read(chunk),
        },
    );
    let _ = read_exchange(&peer);
    peer.write_all(&vec![b'x'; MAX_LINE + 1]).unwrap();
    assert_eq!(
        deadline.recv_or_fail(&result, "over-long prefix"),
        Err(MessageError::Unreachable),
        "one byte past the cap"
    );
    assert_closed(&peer);
}

fn send_message(
    home: &Home,
    to: &str,
    from: &str,
    text: String,
    cancel: Arc<CancelToken>,
) -> Receiver<Result<(), MessageError>> {
    let client = home.client();
    let (tx, rx) = mpsc::channel();
    let (to, from) = (to.to_owned(), from.to_owned());
    thread::spawn(move || {
        let result = client.message(
            &SessionId(to),
            &SessionId(from),
            None,
            &text,
            cancel.as_ref(),
        );
        tx.send(result).unwrap();
    });
    rx
}

fn accept_message_peer(listener: UnixListener, deadline: &Deadline) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let mut fds = [PollFd::new(&listener, PollFlags::IN)];
    let bound = rustix::event::Timespec::try_from(deadline.left()).unwrap();
    rustix::event::poll(&mut fds, Some(&bound)).unwrap();
    assert!(
        !fds[0].revents().is_empty(),
        "a message client connects within the deadline"
    );
    let (stream, _) = listener.accept().unwrap();
    // macOS inherits the listener's nonblocking mode; the peer drives a
    // scripted exchange, so it blocks with timeouts.
    stream.set_nonblocking(false).unwrap();
    let timeout = deadline.left().max(Duration::from_millis(1));
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    stream
}

struct Exchange {
    sub: String,
    msg: String,
    text: String,
}

fn read_exchange(stream: &UnixStream) -> (Exchange, BufReader<UnixStream>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut second = String::new();
    reader.read_line(&mut second).unwrap();
    let first: Value = serde_json::from_str(&first).unwrap();
    let second: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(first["command"], "subscribe");
    assert_eq!(first["args"], serde_json::json!({"level":"summary"}));
    assert_eq!(second["command"], "message");
    let sub = first["id"].as_str().unwrap().to_owned();
    let msg = second["id"].as_str().unwrap().to_owned();
    assert!(sub.starts_with("c_"), "subscribe id mints c_: {sub}");
    assert!(msg.starts_with("c_"), "message id mints c_: {msg}");
    assert_ne!(sub, msg, "each command uses a fresh id");
    let from = second["args"]["from_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let text = second["args"]["text"].as_str().unwrap().to_owned();
    assert_eq!(from, FROM);
    (Exchange { sub, msg, text }, reader)
}

fn assert_closed(stream: &UnixStream) {
    let mut one = [0u8; 1];
    let mut reader = stream.try_clone().unwrap();
    match reader.read(&mut one) {
        Ok(0) => {}
        Ok(_) => panic!("the sender writes no third line after its answer"),
        Err(e) => panic!("the sender closes straight after its answer: {e}"),
    }
}

#[test]
fn message_delivers_past_noise_and_closes_at_once() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "the parser is fixed".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (exchange, _) = read_exchange(&stream);
    assert_eq!(exchange.text, "the parser is fixed");
    writeln!(stream, "{}", accepted(&exchange.sub)).unwrap();
    writeln!(stream, "{}", summary(&status("ready"))).unwrap();
    writeln!(
        stream,
        "{}",
        serde_json::json!({"kind":"extensions_loaded", "payload":{}})
    )
    .unwrap();
    writeln!(stream, "not JSON").unwrap();
    writeln!(stream, "{}", accepted(&exchange.msg)).unwrap();
    assert_eq!(deadline.recv_or_fail(&result, "message delivered"), Ok(()));
    assert_closed(&stream);
}

#[test]
fn message_rejections_pass_through_unchanged() {
    for (code, message) in [
        ("closing", "shutting down"),
        ("message_refused", "nope"),
        ("hook_failed", "boom"),
        ("not_subscribed", "wrong level"),
    ] {
        let home = Home::new();
        let cancel = Arc::new(CancelToken::new());
        let deadline = Deadline::after(fakes::deadline::WAITS);
        let listener = home.listener(ID);
        let result = send_message(&home, ID, FROM, "hello".into(), cancel);
        let mut stream = accept_message_peer(listener, &deadline);
        let (exchange, _) = read_exchange(&stream);
        writeln!(stream, "{}", accepted(&exchange.sub)).unwrap();
        writeln!(stream, "{}", rejected(&exchange.msg, code, message)).unwrap();
        let answer = deadline.recv_or_fail(&result, "rejection passes through");
        match answer {
            Err(MessageError::Rejected(failure)) => {
                assert_eq!(
                    failure.code,
                    serde_json::from_value::<contract::ErrorCode>(serde_json::json!(code)).unwrap()
                );
                assert_eq!(failure.message, message);
            }
            other => panic!("a {code} rejection passes through unchanged: {other:?}"),
        }
        assert_closed(&stream);
    }
}

#[test]
fn a_rejected_subscribe_then_an_accepted_message_is_delivered() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (exchange, _) = read_exchange(&stream);
    // The parent-bound socket of a delegate on another harness accepts only
    // `message`, so a subscribe rejection before the message answer is
    // skipped, not reported.
    writeln!(
        stream,
        "{}",
        rejected(&exchange.sub, "not_subscribed", "summary only")
    )
    .unwrap();
    writeln!(stream, "{}", accepted(&exchange.msg)).unwrap();
    assert_eq!(deadline.recv_or_fail(&result, "message delivered"), Ok(()));
    assert_closed(&stream);
}

#[test]
fn message_ids_that_are_not_full_session_ids_touch_nothing() {
    for id in [
        "fix the parser",
        "s_0123",
        "hub",
        "s_0123456789ABCDEF",
        "../x",
        "",
        "s_0123456789abcde",
        "s_0123456789abcdefg",
    ] {
        let home = Home::new();
        let listener = home.listener(ID);
        let cancel = Arc::new(CancelToken::new());
        assert_eq!(
            home.client().message(
                &SessionId(id.into()),
                &SessionId(FROM.into()),
                None,
                "hello",
                cancel.as_ref()
            ),
            Err(MessageError::Unreachable),
            "id {id:?} is unreachable"
        );
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "id {id:?} connects nowhere"
        );
    }
}

#[test]
fn a_live_hub_socket_is_not_a_session() {
    let home = Home::new();
    let listener = home.listener("hub");
    let cancel = Arc::new(CancelToken::new());
    assert_eq!(
        home.client().message(
            &SessionId("hub".into()),
            &SessionId(FROM.into()),
            None,
            "hello",
            cancel.as_ref()
        ),
        Err(MessageError::Unreachable)
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn message_without_a_listener_is_unreachable() {
    let home = Home::new();
    // No `run/` directory at all.
    assert_eq!(
        home.client().message(
            &SessionId(ID.into()),
            &SessionId(FROM.into()),
            None,
            "hello",
            &CancelToken::new()
        ),
        Err(MessageError::Unreachable)
    );
    // `run/` exists but no socket for the id.
    fs::create_dir_all(home.run()).unwrap();
    assert_eq!(
        home.client().message(
            &SessionId(ID.into()),
            &SessionId(FROM.into()),
            None,
            "hello",
            &CancelToken::new()
        ),
        Err(MessageError::Unreachable)
    );
    // A stale socket file: bound, then dropped, never listening.
    {
        let stale = UnixListener::bind(home.run().join(ID)).unwrap();
        drop(stale);
    }
    assert_eq!(
        home.client().message(
            &SessionId(ID.into()),
            &SessionId(FROM.into()),
            None,
            "hello",
            &CancelToken::new()
        ),
        Err(MessageError::Unreachable)
    );
}

#[test]
fn message_closing_before_the_answer_is_unreachable() {
    // End of file before any answer.
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let stream = accept_message_peer(listener, &deadline);
    let (_exchange, _) = read_exchange(&stream);
    drop(stream);
    assert_eq!(
        deadline.recv_or_fail(&result, "EOF before any answer"),
        Err(MessageError::Unreachable)
    );
    // End of file after the subscribe ack only.
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (exchange, _) = read_exchange(&stream);
    writeln!(stream, "{}", accepted(&exchange.sub)).unwrap();
    drop(stream);
    assert_eq!(
        deadline.recv_or_fail(&result, "EOF after the subscribe ack"),
        Err(MessageError::Unreachable)
    );
}

#[test]
fn a_line_over_the_cap_is_unreachable() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (_exchange, _) = read_exchange(&stream);
    let long = "x".repeat(MAX_LINE + 10);
    stream.write_all(long.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    assert_eq!(
        deadline.recv_or_fail(&result, "over-long line"),
        Err(MessageError::Unreachable)
    );
}

#[test]
fn a_mibibyte_of_text_arrives_whole() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let text = "y".repeat(1024 * 1024);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, text.clone(), cancel);
    let stream = accept_message_peer(listener, &deadline);
    // The peer reads slowly, one byte at a time, so the sender's non-blocking
    // writes fill the send buffer at least once on the way to a mebibyte.
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = Vec::new();
    while !first.ends_with(b"\n") {
        let mut one = [0u8; 1];
        reader.read_exact(&mut one).unwrap();
        first.extend_from_slice(&one);
    }
    let mut second = Vec::new();
    while !second.ends_with(b"\n") {
        let mut one = [0u8; 1];
        reader.read_exact(&mut one).unwrap();
        second.extend_from_slice(&one);
    }
    let second: Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["command"], "message");
    assert_eq!(second["args"]["text"].as_str().unwrap(), text);
    assert_eq!(second["args"]["text"].as_str().unwrap().len(), 1024 * 1024);
    let msg = second["id"].as_str().unwrap().to_owned();
    let mut stream = stream;
    writeln!(stream, "{}", accepted(&msg)).unwrap();
    assert_eq!(deadline.recv_or_fail(&result, "mebibyte delivered"), Ok(()));
    assert_closed(&stream);
}

#[test]
fn message_waits_for_the_answer_until_cancelled() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), Arc::clone(&cancel));
    let stream = accept_message_peer(listener, &deadline);
    let (_exchange, _) = read_exchange(&stream);
    // The listener accepts and never answers; the call holds until the
    // turn is cancelled, then the listener reads end of file.
    assert!(
        home.clock
            .await_parked_unbounded(fakes::Deadline::after(fakes::deadline::WAITS))
    );
    cancel.cancel();
    assert_eq!(
        deadline.recv_or_fail(&result, "cancelled wait"),
        Err(MessageError::Cancelled)
    );
    assert_closed(&stream);
}

#[test]
fn a_cancelled_call_connects_nowhere() {
    let home = Home::new();
    let listener = home.listener(ID);
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        home.client().message(
            &SessionId(ID.into()),
            &SessionId(FROM.into()),
            None,
            "hello",
            &cancel
        ),
        Err(MessageError::Cancelled)
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn the_sender_checks_no_schema_version() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (exchange, _) = read_exchange(&stream);
    let mut ack = accepted(&exchange.sub);
    ack["schema_version"] = serde_json::json!(contract::SCHEMA_VERSION + 1);
    writeln!(stream, "{ack}").unwrap();
    writeln!(stream, "{}", accepted(&exchange.msg)).unwrap();
    assert_eq!(deadline.recv_or_fail(&result, "version unchecked"), Ok(()));
    assert_closed(&stream);
}

#[test]
fn answers_for_another_command_are_skipped() {
    let home = Home::new();
    let cancel = Arc::new(CancelToken::new());
    let deadline = Deadline::after(fakes::deadline::WAITS);
    let listener = home.listener(ID);
    let result = send_message(&home, ID, FROM, "hello".into(), cancel);
    let mut stream = accept_message_peer(listener, &deadline);
    let (exchange, _) = read_exchange(&stream);
    writeln!(stream, "{}", accepted(&exchange.sub)).unwrap();
    writeln!(stream, "{}", accepted("c_someone_else")).unwrap();
    writeln!(
        stream,
        "{}",
        rejected("c_someone_else", "closing", "not ours")
    )
    .unwrap();
    writeln!(stream, "{}", accepted(&exchange.msg)).unwrap();
    assert_eq!(
        deadline.recv_or_fail(&result, "other command skipped"),
        Ok(())
    );
    assert_closed(&stream);
}

#[test]
fn session_entries_lists_only_session_id_names_sorted() {
    let dir = TempDir::new("doors-session-entries");
    let run = dir.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("s_ffffffffffffffff"), b"x").unwrap();
    std::fs::write(run.join("s_0000000000000001"), b"x").unwrap();
    std::fs::write(run.join("hub"), b"x").unwrap();
    std::fs::write(run.join("s_short"), b"x").unwrap();
    std::fs::write(run.join("notes.md"), b"x").unwrap();
    assert_eq!(
        session_entries(dir.path()).unwrap(),
        [
            SessionId("s_0000000000000001".to_owned()),
            SessionId("s_ffffffffffffffff".to_owned()),
        ]
    );
}

#[test]
fn session_entries_is_empty_for_a_missing_run_dir() {
    let dir = TempDir::new("doors-session-entries-missing");
    assert_eq!(session_entries(dir.path()).unwrap(), vec![]);
}

#[test]
fn session_entries_propagates_a_non_not_found_error() {
    let dir = TempDir::new("doors-session-entries-file");
    std::fs::write(dir.path().join("run"), b"not a directory").unwrap();
    let error = session_entries(dir.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
}
